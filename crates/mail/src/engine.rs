//! The engine the apps drive: adding accounts, and per account one round of
//! work at a time (send what's due, push local changes, fetch new mail,
//! reach back for older mail, list and sync folders). Callers run rounds
//! for an account one after another; different accounts can run in
//! parallel.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use lettre::message::Mailbox;
use serde::{Deserialize, Serialize};
use spark_store::{Account, AccountKind, Folder, Location, NewAccount, Store, now_ms};

use crate::attachment::{self, AttachmentData, InlineImage};
use crate::calendar::{Event, Response};
use crate::draft::{self, Draft, Original, OutgoingAttachment};
use crate::drafts::{self, DraftsReport};
use crate::graph::{self, Graph};
use crate::imap::{self, Server};
use crate::invite::{self, Answer, Invite};
use crate::oauth::{self, App};
use crate::{Error, OLDER_STEP, Result, smtp};

/// How long a local change waits before it's pushed: longer than the UI's
/// undo window, so an undone change never reaches the server.
pub const SETTLE_MS: i64 = 6_000;
/// A send that failed on the network is retried after this long.
const SEND_RETRY_MS: i64 = 60_000;

/// Where passwords and refresh tokens live (the OS credential store in the
/// apps). An empty value deletes.
pub trait Secrets: Send + Sync {
    fn get(&self, name: &str) -> Result<Option<String>, String>;
    fn set(&self, name: &str, value: &str) -> Result<(), String>;
}

pub fn password_key(account_id: i64) -> String {
    format!("account:{account_id}:password")
}

pub fn token_key(account_id: i64) -> String {
    format!("account:{account_id}:refresh-token")
}

/// Backend settings, stored as the account's `config` JSON.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AccountConfig {
    #[serde(rename_all = "camelCase")]
    Imap {
        username: String,
        imap: Server,
        smtp: Server,
    },
    #[serde(rename_all = "camelCase")]
    Microsoft {
        client_id: String,
        tenant: String,
        /// Signed in with the calendar permission (`oauth::CALENDAR_SCOPE`).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        calendar: bool,
    },
}

/// What a round does besides pushing local changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Round {
    /// Fetch new inbox mail.
    pub fetch: bool,
    /// Refresh the account's folder list.
    pub list_folders: bool,
    /// Stored folders (of any account; others are skipped) to sync.
    pub folders: Vec<i64>,
    /// Reach `OLDER_STEP` messages further back in the inbox.
    pub older_inbox: bool,
    /// Stored folders (of any account; others are skipped) to reach
    /// `OLDER_STEP` messages further back in, and so sync.
    pub older_folders: Vec<i64>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub new: usize,
    pub gone: usize,
    pub pushed: usize,
    /// Ids of the new messages worth telling the user about: all of them,
    /// except on an account's first sync, which brings in mail they've
    /// already seen elsewhere.
    pub arrived: Vec<i64>,
    /// Older inbox mail brought in by reaching back. It's never `arrived`.
    pub older: usize,
    /// Messages that came into or left synced folders.
    pub folder_changes: usize,
    /// The folder list changed, a folder was synced for the first time, or
    /// one now has (or no longer has) older mail beyond its list.
    pub folders_changed: bool,
    /// Folders that couldn't be synced this round, and why.
    pub folder_errors: Vec<String>,
    /// Drafts uploaded to the server, and server copies deleted.
    pub drafts: usize,
}

impl Report {
    /// Whether the message list needs reloading.
    pub fn changed(&self) -> bool {
        self.new + self.gone + self.older + self.folder_changes + self.drafts > 0
    }

    fn drafts_pushed(&mut self, pushed: DraftsReport) {
        self.drafts = pushed.uploaded + pushed.removed;
        self.folders_changed |= pushed.folders_changed;
        self.folder_errors
            .extend(pushed.errors.into_iter().map(|e| format!("Drafts: {e}")));
    }

    fn synced(&mut self, new: Vec<i64>, gone: usize, first: bool) {
        self.new = new.len();
        self.gone = gone;
        if !first {
            self.arrived = new;
        }
    }
}

/// The outcome of one queued draft.
#[derive(Debug)]
pub struct SendOutcome {
    pub id: i64,
    /// `Ok(true)` sent; `Ok(false)` will be retried; `Err` failed for good.
    pub result: Result<bool>,
}

/// Whether an account's calendar can show.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum CalendarStatus {
    Ready,
    /// Signed in before the calendar permission was asked for, or Microsoft
    /// wants the user to sign in again.
    SignIn,
    /// Gmail and IMAP accounts, for now.
    Unsupported,
    /// The last refresh failed.
    Error {
        message: String,
    },
}

/// The longest stretch of calendar fetched at once.
const CALENDAR_STRETCH_MS: i64 = 400 * 86_400_000;

pub struct Engine {
    store: Arc<Mutex<Store>>,
    secrets: Arc<dyn Secrets>,
    /// Microsoft access tokens and their expiry (Unix ms), in memory only.
    tokens: Mutex<HashMap<i64, (String, i64)>>,
    /// How each account's last calendar refresh went.
    calendars: Mutex<HashMap<i64, CalendarStatus>>,
}

impl Engine {
    pub fn new(store: Arc<Mutex<Store>>, secrets: Arc<dyn Secrets>) -> Self {
        Self {
            store,
            secrets,
            tokens: Mutex::new(HashMap::new()),
            calendars: Mutex::new(HashMap::new()),
        }
    }

    pub fn store(&self) -> MutexGuard<'_, Store> {
        self.store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Adds an IMAP account after checking that the password works.
    pub async fn add_password_account(
        &self,
        kind: AccountKind,
        email: &str,
        password: &str,
        label: &str,
        imap: Server,
        smtp: Server,
    ) -> Result<Account> {
        let email = email.trim();
        if !email.contains('@') {
            return Err(Error::Config("Enter your full email address.".into()));
        }
        // Google shows app passwords in groups of four with spaces.
        let password: String = if kind == AccountKind::Gmail {
            password.split_whitespace().collect()
        } else {
            password.trim().to_owned()
        };
        let mut session = imap::login(&imap, email, &password).await.map_err(|e| match e {
            Error::Auth(_) if kind == AccountKind::Gmail => Error::Auth(
                "Google didn't accept that app password. Check the address, and create a new app password if needed."
                    .into(),
            ),
            Error::Auth(_) => Error::Auth("The server didn't accept that email and password.".into()),
            other => other,
        })?;
        let _ = session.logout().await;

        let config = AccountConfig::Imap {
            username: email.to_owned(),
            imap,
            smtp,
        };
        let account = self.store().add_account(&NewAccount {
            kind,
            email,
            name: "",
            label: &default_label(label, kind, email),
            config: &to_json(&config),
        })?;
        if let Err(e) = self.secrets.set(&password_key(account.id), &password) {
            self.store().remove_account(account.id)?;
            return Err(Error::Config(format!("Couldn't save the password: {e}")));
        }
        Ok(account)
    }

    /// Signs in to Microsoft in the browser and adds the account.
    pub async fn add_microsoft_account(
        &self,
        client_id: &str,
        tenant: &str,
        label: &str,
        open_browser: impl FnOnce(&str) -> Result<(), String>,
    ) -> Result<Account> {
        let app = App::new(client_id, tenant)?;
        let tokens = oauth::sign_in(&app, None, open_browser).await?;
        let me = Graph::new(&tokens.access_token).me().await?;
        let email = me
            .address()
            .ok_or_else(|| Error::Server("Microsoft didn't say which mailbox this is.".into()))?
            .to_owned();
        let refresh = tokens.refresh_token.clone().ok_or_else(|| {
            Error::Auth("Microsoft didn't allow staying signed in (no refresh token).".into())
        })?;
        let config = AccountConfig::Microsoft {
            client_id: app.client_id.clone(),
            tenant: app.tenant.clone(),
            calendar: true,
        };
        let account = self.store().add_account(&NewAccount {
            kind: AccountKind::Microsoft,
            email: &email,
            name: me.display_name.as_deref().unwrap_or_default(),
            label: &default_label(label, AccountKind::Microsoft, &email),
            config: &to_json(&config),
        })?;
        if let Err(e) = self.secrets.set(&token_key(account.id), &refresh) {
            self.store().remove_account(account.id)?;
            return Err(Error::Config(format!("Couldn't save the sign-in: {e}")));
        }
        self.cache_token(account.id, &tokens);
        Ok(account)
    }

    /// Signs in to Microsoft again for an existing account, asking for the
    /// calendar permission as new sign-ins do, and keeps the new sign-in.
    /// It must be the same mailbox.
    pub async fn reconnect_microsoft_account(
        &self,
        account_id: i64,
        open_browser: impl FnOnce(&str) -> Result<(), String>,
    ) -> Result<Account> {
        let account = self.account(account_id)?;
        let AccountConfig::Microsoft {
            client_id, tenant, ..
        } = config(&account)?
        else {
            return Err(Error::Config(
                "Only Microsoft 365 accounts sign in this way.".into(),
            ));
        };
        let app = App::new(&client_id, &tenant)?;
        let tokens = oauth::sign_in(&app, Some(&account.email), open_browser).await?;
        let me = Graph::new(&tokens.access_token).me().await?;
        if !me
            .address()
            .is_some_and(|a| a.eq_ignore_ascii_case(&account.email))
        {
            return Err(Error::Auth(format!(
                "That was another account. Sign in as {}.",
                account.email
            )));
        }
        let refresh = tokens.refresh_token.clone().ok_or_else(|| {
            Error::Auth("Microsoft didn't allow staying signed in (no refresh token).".into())
        })?;
        self.secrets
            .set(&token_key(account_id), &refresh)
            .map_err(|e| Error::Config(format!("Couldn't save the sign-in: {e}")))?;
        let config = AccountConfig::Microsoft {
            client_id,
            tenant,
            calendar: true,
        };
        self.store()
            .set_account_config(account_id, &to_json(&config))?;
        self.cache_token(account_id, &tokens);
        self.calendars_lock().remove(&account_id);
        self.account(account_id)
    }

    /// Removes the account, its mail and its saved password or sign-in.
    pub fn remove_account(&self, account_id: i64) -> Result<()> {
        self.store().remove_account(account_id)?;
        self.tokens_lock().remove(&account_id);
        let _ = self.secrets.set(&password_key(account_id), "");
        let _ = self.secrets.set(&token_key(account_id), "");
        Ok(())
    }

    /// One round for an account: push changes older than the undo window,
    /// and with `fetch`, bring in new mail. Opens no connection when there
    /// is nothing to do.
    pub async fn run(&self, account_id: i64, fetch: bool) -> Result<Report> {
        self.run_round(
            account_id,
            &Round {
                fetch,
                ..Default::default()
            },
        )
        .await
    }

    /// `run`, plus reaching back for older mail, listing the account's
    /// folders and syncing some of them. A folder that can't be synced
    /// doesn't fail the round unless the account itself is unreachable.
    ///
    /// Reaching back for older inbox mail moves the sync position, which a
    /// round reads at its start and writes at its end: so it happens here,
    /// in the account's round, and not on a connection of its own.
    pub async fn run_round(&self, account_id: i64, round: &Round) -> Result<Report> {
        let account = self.account(account_id)?;
        let pending = self.store().pending(account_id, now_ms(), SETTLE_MS)?;
        // Saves settle like other changes, so a burst of them goes up once.
        let settled = now_ms() - SETTLE_MS;
        let draft_work = self.store().has_draft_work(account_id, settled)?;
        let folders: Vec<Folder> = self
            .store()
            .folders()?
            .into_iter()
            .filter(|f| {
                f.account_id == account_id
                    && f.selectable
                    && (round.folders.contains(&f.id) || round.older_folders.contains(&f.id))
            })
            .collect();
        if pending.is_empty()
            && !draft_work
            && !round.fetch
            && !round.older_inbox
            && !round.list_folders
            && folders.is_empty()
        {
            return Ok(Report::default());
        }
        let mut report = Report::default();
        match config(&account)? {
            AccountConfig::Imap {
                username,
                imap: server,
                ..
            } => {
                let password = self.password(account_id)?;
                let mut state: imap::SyncState = from_json(&account.sync_state);
                let mut session = imap::login(&server, &username, &password).await?;
                imap::find_folders(&mut session, &mut state).await?;
                report.pushed = imap::push(&mut session, &self.store, &state, pending).await?;
                if draft_work {
                    let pushed =
                        drafts::push_imap(&mut session, &self.store, &account, &state, settled)
                            .await?;
                    report.drafts_pushed(pushed);
                }
                if round.fetch {
                    let sync =
                        imap::sync_inbox(&mut session, &self.store, account_id, &mut state).await;
                    // Keep the position even if part of the sync failed.
                    self.store().set_sync_state(account_id, &to_json(&state))?;
                    let sync = sync?;
                    report.synced(sync.new, sync.gone, sync.first);
                }
                if round.older_inbox {
                    let older =
                        imap::older_inbox(&mut session, &self.store, account_id, &mut state).await;
                    // Keep the lowered floor even if part of it failed.
                    self.store().set_sync_state(account_id, &to_json(&state))?;
                    report.older = older?;
                }
                self.store().set_sync_state(account_id, &to_json(&state))?;
                let mut listed = false;
                if round.list_folders {
                    let found = imap::list_folders(&mut session, &state).await?;
                    report.folders_changed |= self.store().replace_folders(account_id, &found)?;
                    listed = true;
                }
                for folder in &folders {
                    let mut folder_state: imap::FolderState = from_json(&folder.sync_state);
                    if round.older_folders.contains(&folder.id) && !folder_state.complete {
                        let limit = folder_state.limit.unwrap_or(imap::FOLDER_LIMIT);
                        folder_state.limit = Some(limit + OLDER_STEP as u32);
                    }
                    match imap::sync_folder(&mut session, &self.store, folder, &mut folder_state)
                        .await
                    {
                        Ok(r) if r.missing => {
                            if !listed {
                                let found = imap::list_folders(&mut session, &state).await?;
                                self.store().replace_folders(account_id, &found)?;
                                listed = true;
                            }
                            report.folders_changed = true;
                        }
                        Ok(r) => {
                            self.folder_synced(folder, &to_json(&folder_state), &mut report)?;
                            report.folder_changes += r.changed;
                        }
                        Err(e) => folder_failed(folder, e, &mut report)?,
                    }
                }
                let _ = session.logout().await;
            }
            AccountConfig::Microsoft {
                client_id, tenant, ..
            } => {
                let graph = Graph::new(
                    &self
                        .access_token(account_id, &App::new(&client_id, &tenant)?)
                        .await?,
                );
                report.pushed = graph.push(&self.store, pending).await?;
                if draft_work {
                    let pushed = drafts::push_graph(&graph, &self.store, &account, settled).await?;
                    report.drafts_pushed(pushed);
                }
                let mut state: graph::SyncState = from_json(&account.sync_state);
                if round.fetch {
                    let sync = graph
                        .sync_inbox(&self.store, account_id, &mut state)
                        .await?;
                    self.store().set_sync_state(account_id, &to_json(&state))?;
                    report.synced(sync.new, sync.gone, sync.first);
                }
                if round.older_inbox {
                    let older = graph.older_inbox(&self.store, account_id, &mut state).await;
                    // Keep how far it got even if part of it failed.
                    self.store().set_sync_state(account_id, &to_json(&state))?;
                    report.older = older?;
                }
                let mut listed = false;
                if round.list_folders {
                    let found = graph.list_folders().await?;
                    report.folders_changed |= self.store().replace_folders(account_id, &found)?;
                    listed = true;
                }
                for folder in &folders {
                    let mut folder_state: graph::FolderState = from_json(&folder.sync_state);
                    if round.older_folders.contains(&folder.id) && !folder_state.complete {
                        let limit = folder_state.limit.unwrap_or(graph::FOLDER_LIMIT);
                        folder_state.limit = Some(limit + OLDER_STEP);
                    }
                    match graph
                        .sync_folder(&self.store, folder, &mut folder_state)
                        .await
                    {
                        Ok(Some(changed)) => {
                            self.folder_synced(folder, &to_json(&folder_state), &mut report)?;
                            report.folder_changes += changed;
                        }
                        Ok(None) => {
                            if !listed {
                                let found = graph.list_folders().await?;
                                self.store().replace_folders(account_id, &found)?;
                                listed = true;
                            }
                            report.folders_changed = true;
                        }
                        Err(e) => folder_failed(folder, e, &mut report)?,
                    }
                }
            }
        }
        Ok(report)
    }

    fn folder_synced(&self, folder: &Folder, state: &str, report: &mut Report) -> Result<()> {
        self.store().set_folder_synced(folder.id, state, now_ms())?;
        let complete = |state: &str| from_json::<Reach>(state).complete;
        if folder.synced_at.is_none() || complete(&folder.sync_state) != complete(state) {
            report.folders_changed = true;
        }
        Ok(())
    }

    /// Creates a folder in the account, in `parent` (a stored folder) or at
    /// the top, and returns the new folder's id once it's listed.
    pub async fn create_folder(
        &self,
        account_id: i64,
        parent: Option<i64>,
        name: &str,
    ) -> Result<i64> {
        let name = folder_name(name)?;
        let account = self.account(account_id)?;
        let parent = parent.map(|id| self.folder(id)).transpose()?;
        let remote_id = match config(&account)? {
            AccountConfig::Imap {
                username,
                imap: server,
                ..
            } => {
                let mut session =
                    imap::login(&server, &username, &self.password(account_id)?).await?;
                let state: imap::SyncState = from_json(&account.sync_state);
                let created = imap::create_folder(
                    &mut session,
                    parent.as_ref().map(|p| p.remote_id.as_str()),
                    name,
                )
                .await;
                let listed = match &created {
                    Ok(_) => Some(imap::list_folders(&mut session, &state).await),
                    Err(_) => None,
                };
                let _ = session.logout().await;
                let remote_id = created?;
                if let Some(listed) = listed {
                    self.store().replace_folders(account_id, &listed?)?;
                }
                remote_id
            }
            AccountConfig::Microsoft {
                client_id, tenant, ..
            } => {
                let graph = self.graph(account_id, &client_id, &tenant).await?;
                let remote_id = graph
                    .create_folder(parent.as_ref().map(|p| p.remote_id.as_str()), name)
                    .await?;
                let listed = graph.list_folders().await?;
                self.store().replace_folders(account_id, &listed)?;
                remote_id
            }
        };
        self.store()
            .folders()?
            .into_iter()
            .find(|f| f.account_id == account_id && f.remote_id == remote_id)
            .map(|f| f.id)
            .ok_or_else(|| Error::Server("The new folder didn't show up on the server.".into()))
    }

    /// Renames a folder on its server, keeping where it is.
    pub async fn rename_folder(&self, folder_id: i64, name: &str) -> Result<()> {
        let name = folder_name(name)?;
        let folder = self.folder(folder_id)?;
        let account = self.account(folder.account_id)?;
        let remote_id = match config(&account)? {
            AccountConfig::Imap {
                username,
                imap: server,
                ..
            } => {
                let mut session =
                    imap::login(&server, &username, &self.password(account.id)?).await?;
                let renamed = imap::rename_folder(&mut session, &folder.remote_id, name).await;
                let _ = session.logout().await;
                renamed?
            }
            AccountConfig::Microsoft {
                client_id, tenant, ..
            } => {
                self.graph(account.id, &client_id, &tenant)
                    .await?
                    .rename_folder(&folder.remote_id, name)
                    .await?;
                folder.remote_id.clone()
            }
        };
        self.store().rename_folder(folder_id, &remote_id, name)?;
        Ok(())
    }

    /// Deletes a folder and the folders in it on the server.
    pub async fn delete_folder(&self, folder_id: i64) -> Result<()> {
        let folder = self.folder(folder_id)?;
        let account = self.account(folder.account_id)?;
        match config(&account)? {
            AccountConfig::Imap {
                username,
                imap: server,
                ..
            } => {
                // Innermost first: not every server deletes a folder with children.
                let all = self.store().folders()?;
                let mut subtree = vec![folder.clone()];
                let mut i = 0;
                while i < subtree.len() {
                    let parent = subtree[i].id;
                    subtree.extend(all.iter().filter(|f| f.parent_id == Some(parent)).cloned());
                    i += 1;
                }
                let mailboxes: Vec<String> =
                    subtree.into_iter().rev().map(|f| f.remote_id).collect();
                let mut session =
                    imap::login(&server, &username, &self.password(account.id)?).await?;
                let deleted = imap::delete_folders(&mut session, &mailboxes).await;
                let _ = session.logout().await;
                deleted?;
            }
            AccountConfig::Microsoft {
                client_id, tenant, ..
            } => {
                self.graph(account.id, &client_id, &tenant)
                    .await?
                    .delete_folder(&folder.remote_id)
                    .await?;
            }
        }
        self.store().remove_folder(folder_id)?;
        Ok(())
    }

    fn folder(&self, folder_id: i64) -> Result<Folder> {
        self.store()
            .folder(folder_id)?
            .ok_or_else(|| Error::Config("That folder is gone.".into()))
    }

    async fn graph(&self, account_id: i64, client_id: &str, tenant: &str) -> Result<Graph> {
        Ok(Graph::new(
            &self
                .access_token(account_id, &App::new(client_id, tenant)?)
                .await?,
        ))
    }

    /// Sends the account's drafts whose time has come.
    pub async fn send_due(&self, account_id: i64) -> Result<Vec<SendOutcome>> {
        let due = self.store().claim_due_sends(account_id, now_ms())?;
        let mut outcomes = Vec::new();
        for (id, json) in due {
            let parsed = serde_json::from_str::<Draft>(&json);
            let saved_draft = parsed.as_ref().ok().and_then(|d| d.draft_id);
            let result = match parsed {
                Ok(draft) => self.send_draft(account_id, id, &draft).await,
                Err(e) => Err(Error::Config(format!(
                    "The queued message is unreadable: {e}"
                ))),
            };
            let result = match result {
                Ok(()) => {
                    // The saved draft it came from is done with, here and on the server.
                    if let Some(saved) = saved_draft {
                        self.store().discard_draft(saved)?;
                    }
                    self.store().finish_send(id)?;
                    Ok(true)
                }
                // Offline or the server is busy: keep it queued.
                Err(Error::Network { .. }) => {
                    self.store().retry_send(id, now_ms() + SEND_RETRY_MS)?;
                    Ok(false)
                }
                Err(e) => {
                    self.store().finish_send(id)?;
                    Err(e)
                }
            };
            outcomes.push(SendOutcome { id, result });
        }
        Ok(outcomes)
    }

    async fn send_draft(&self, account_id: i64, outbox_id: i64, draft: &Draft) -> Result<()> {
        let account = self.account(account_id)?;
        let original = draft
            .reply_to
            .map(|id| self.original(id))
            .transpose()?
            .flatten()
            .or_else(|| drafts::original_from_headers(draft));
        let files = self.outgoing_files(outbox_id, draft).await?;
        match config(&account)? {
            AccountConfig::Imap {
                username,
                smtp: server,
                ..
            } => {
                let from = Mailbox::new(
                    Some(account.name.clone()).filter(|n| !n.is_empty()),
                    account
                        .email
                        .parse()
                        .map_err(|_| Error::Config("The account's address is invalid.".into()))?,
                );
                let message =
                    draft::build_message(from, draft, original.as_ref().map(|(o, _)| o), &files)?;
                smtp::send(&server, &username, &self.password(account_id)?, message).await?;
            }
            AccountConfig::Microsoft {
                client_id, tenant, ..
            } => {
                let token = self
                    .access_token(account_id, &App::new(&client_id, &tenant)?)
                    .await?;
                let remote = original.as_ref().and_then(|(_, remote)| remote.as_deref());
                Graph::new(&token).send_draft(draft, remote, &files).await?;
            }
        }
        // People you write to skip the Gatekeeper.
        let store = self.store();
        for field in [&draft.to, &draft.cc, &draft.bcc] {
            for m in draft::recipients(field).unwrap_or_default() {
                store.accept_sender(account_id, m.email.as_ref())?;
            }
        }
        Ok(())
    }

    /// Whether the account's calendar can show, as of its last refresh.
    pub fn calendar_status(&self, account: &Account) -> CalendarStatus {
        match config(account) {
            Ok(AccountConfig::Microsoft { calendar: true, .. }) => self
                .calendars_lock()
                .get(&account.id)
                .cloned()
                .unwrap_or(CalendarStatus::Ready),
            Ok(AccountConfig::Microsoft { .. }) => CalendarStatus::SignIn,
            Ok(AccountConfig::Imap { .. }) => CalendarStatus::Unsupported,
            Err(e) => CalendarStatus::Error {
                message: e.to_string(),
            },
        }
    }

    /// Fetches the account's calendars and their events in [start, end)
    /// (Unix ms, up to about a year at once) into the store. Accounts
    /// without a calendar are skipped. The outcome is kept for
    /// `calendar_status`; a calendar that can't be read (a shared one taken
    /// away) is left as it was.
    pub async fn refresh_calendar(&self, account_id: i64, start: i64, end: i64) -> Result<()> {
        let account = self.account(account_id)?;
        let AccountConfig::Microsoft {
            client_id,
            tenant,
            calendar: true,
        } = config(&account)?
        else {
            return Ok(());
        };
        let end = end.min(start + CALENDAR_STRETCH_MS);
        let result = async {
            let graph = self.graph(account_id, &client_id, &tenant).await?;
            let listed = graph.calendars().await?;
            self.store().replace_calendars(account_id, &listed)?;
            let calendars = self.store().calendars()?;
            for c in calendars.iter().filter(|c| c.account_id == account_id) {
                match graph.events(&c.remote_id, start, end).await {
                    Ok(events) => self.store().replace_events(c.id, start, end, &events)?,
                    Err(Error::Gone | Error::Auth(_)) => {}
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        }
        .await;
        let status = match &result {
            Ok(()) => CalendarStatus::Ready,
            Err(Error::Auth(_)) => CalendarStatus::SignIn,
            Err(e) => CalendarStatus::Error {
                message: e.to_string(),
            },
        };
        self.calendars_lock().insert(account_id, status);
        result
    }

    /// Answers a stored event in its calendar, telling the organizer if
    /// they asked to hear back, and keeps the answer.
    pub async fn answer_event(&self, event_id: i64, answer: Answer) -> Result<()> {
        let stored = self
            .store()
            .event(event_id)?
            .ok_or_else(|| Error::Config("That event is no longer here.".into()))?;
        let account = self.account(stored.account_id)?;
        let AccountConfig::Microsoft {
            client_id, tenant, ..
        } = config(&account)?
        else {
            return Err(Error::Config(
                "Only Microsoft 365 events can be answered here.".into(),
            ));
        };
        let mut event = Event::from_json(&stored.data);
        self.graph(account.id, &client_id, &tenant)
            .await?
            .answer_event(&stored.remote_id, answer, event.response_requested)
            .await?;
        event.response = match answer {
            Answer::Accepted => Response::Accepted,
            Answer::Tentative => Response::Tentative,
            Answer::Declined => Response::Declined,
        };
        self.store().set_event_data(event_id, &event.to_json())?;
        Ok(())
    }

    /// Answers the invitation message `message_id` carries, by email: a
    /// REPLY to its organizer, sent at once rather than through the outbox.
    /// The answer is kept with the message for its card.
    pub async fn answer_invite(&self, message_id: i64, answer: Answer) -> Result<()> {
        let (account_id, invite, original) = {
            let store = self.store();
            let gone = || Error::Config("That email is no longer here.".into());
            let message = store.message(message_id)?.ok_or_else(gone)?;
            let content = store.content(message_id)?.ok_or_else(gone)?;
            let headers = store.headers(message_id)?.unwrap_or_default();
            (message.account_id, content.invite, headers.message_id)
        };
        let account = self.account(account_id)?;
        let not_an_invitation = || Error::Config("That invitation can't be answered.".into());
        let invite = invite
            .as_deref()
            .and_then(Invite::from_json)
            .ok_or_else(not_an_invitation)?;
        let organizer = invite.organizer.as_ref().ok_or_else(not_an_invitation)?;
        let reply = invite::reply(&invite.ics, &account.email, &account.name, answer, now_ms())
            .ok_or_else(not_an_invitation)?;
        let address = |a: &str| -> Result<lettre::Address> {
            a.parse()
                .map_err(|_| Error::Config(format!("\"{a}\" isn't an email address.")))
        };
        let from = Mailbox::new(
            Some(account.name.clone()).filter(|n| !n.is_empty()),
            address(&account.email)?,
        );
        let to = Mailbox::new(
            Some(organizer.name.clone()).filter(|n| n != &organizer.address),
            address(&organizer.address)?,
        );
        let message = invite::reply_message(
            from,
            to,
            &invite.summary,
            answer,
            &reply,
            original.as_deref(),
        )?;
        match config(&account)? {
            AccountConfig::Imap {
                username,
                smtp: server,
                ..
            } => smtp::send(&server, &username, &self.password(account.id)?, message).await?,
            AccountConfig::Microsoft {
                client_id, tenant, ..
            } => {
                let token = self
                    .access_token(account.id, &App::new(&client_id, &tenant)?)
                    .await?;
                Graph::new(&token).send_mime(&message.formatted()).await?;
            }
        }
        self.store().set_rsvp(message_id, answer.as_str())?;
        Ok(())
    }

    /// The draft's attachments in order: the copies queued with it, and
    /// forwarded ones fetched again from the server.
    async fn outgoing_files(&self, outbox_id: i64, draft: &Draft) -> Result<Vec<AttachmentData>> {
        let mut queued = self.store().outbox_files(outbox_id)?.into_iter();
        let mut files = Vec::with_capacity(draft.attachments.len());
        for a in &draft.attachments {
            match a {
                OutgoingAttachment::File { name, .. } => {
                    let f = queued.next().ok_or_else(|| {
                        Error::Config(format!("{name} is missing from the outbox."))
                    })?;
                    files.push(AttachmentData {
                        name: f.name,
                        mime: f.mime,
                        bytes: f.data,
                    });
                }
                OutgoingAttachment::Message { message, index, .. } => {
                    files.push(self.attachment(*message, *index).await?);
                }
            }
        }
        Ok(files)
    }

    /// Downloads attachment `index` of a stored message from its server.
    pub async fn attachment(&self, message_id: i64, index: usize) -> Result<AttachmentData> {
        let (at, wanted) = {
            let store = self.store();
            let gone = || Error::Config("That email is no longer here.".into());
            let at = store.location(message_id)?.ok_or_else(gone)?;
            let content = store.content(message_id)?.ok_or_else(gone)?;
            let wanted =
                content.attachments.get(index).cloned().ok_or_else(|| {
                    Error::Config("That attachment isn't part of the email.".into())
                })?;
            (at, wanted)
        };
        let account = self.account(at.account_id)?;
        match config(&account)? {
            AccountConfig::Imap {
                username,
                imap: server,
                ..
            } => {
                let raw = self.fetch_raw(&account, &server, &username, &at).await?;
                attachment::extract(&raw, index, &wanted.name).ok_or_else(|| {
                    Error::Server(format!(
                        "{} wasn't in the email on the server.",
                        wanted.name
                    ))
                })
            }
            AccountConfig::Microsoft {
                client_id, tenant, ..
            } => {
                let remote = at.remote_id.ok_or(Error::Gone)?;
                let token = self
                    .access_token(account.id, &App::new(&client_id, &tenant)?)
                    .await?;
                Graph::new(&token).attachment(&remote, index, &wanted).await
            }
        }
    }

    /// The images a stored message's HTML embeds with `cid:` URLs, from its
    /// server. Empty, without asking the server, when it embeds none.
    pub async fn inline_images(&self, message_id: i64) -> Result<Vec<InlineImage>> {
        let (at, wanted) = {
            let store = self.store();
            let gone = || Error::Config("That email is no longer here.".into());
            let at = store.location(message_id)?.ok_or_else(gone)?;
            let content = store.content(message_id)?.ok_or_else(gone)?;
            let wanted = content
                .html
                .as_deref()
                .map(attachment::cid_refs)
                .unwrap_or_default();
            (at, wanted)
        };
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let account = self.account(at.account_id)?;
        match config(&account)? {
            AccountConfig::Imap {
                username,
                imap: server,
                ..
            } => {
                let raw = self.fetch_raw(&account, &server, &username, &at).await?;
                Ok(attachment::inline_images(&raw, &wanted))
            }
            AccountConfig::Microsoft {
                client_id, tenant, ..
            } => {
                let remote = at.remote_id.ok_or(Error::Gone)?;
                let token = self
                    .access_token(account.id, &App::new(&client_id, &tenant)?)
                    .await?;
                Graph::new(&token).inline_images(&remote, &wanted).await
            }
        }
    }

    /// A stored message as the IMAP server has it, wherever it is now.
    async fn fetch_raw(
        &self,
        account: &Account,
        server: &Server,
        username: &str,
        at: &Location,
    ) -> Result<Vec<u8>> {
        let password = self.password(account.id)?;
        let state: imap::SyncState = from_json(&account.sync_state);
        let mut session = imap::login(server, username, &password).await?;
        let raw = imap::fetch_raw(&mut session, &state, at).await;
        let _ = session.logout().await;
        raw?.ok_or(Error::Gone)
    }

    /// The message being replied to, and its Graph id if it has one.
    fn original(&self, id: i64) -> Result<Option<(Original, Option<String>)>> {
        let store = self.store();
        let (Some(message), Some(headers), Some(content)) =
            (store.message(id)?, store.headers(id)?, store.content(id)?)
        else {
            return Ok(None);
        };
        Ok(Some((
            Original {
                message_id: headers.message_id,
                references: headers.references,
                from: message.from_name,
                text: content.text,
            },
            headers.remote_id,
        )))
    }

    fn account(&self, account_id: i64) -> Result<Account> {
        self.store()
            .account(account_id)?
            .ok_or_else(|| Error::Config("That account was removed.".into()))
    }

    fn password(&self, account_id: i64) -> Result<String> {
        self.secrets
            .get(&password_key(account_id))
            .map_err(|e| Error::Config(format!("Couldn't read the saved password: {e}")))?
            .ok_or_else(|| {
                Error::Auth(
                    "The saved password is missing. Remove the account and add it again.".into(),
                )
            })
    }

    async fn access_token(&self, account_id: i64, app: &App) -> Result<String> {
        if let Some((token, expires_at)) = self.tokens_lock().get(&account_id)
            && *expires_at - 120_000 > now_ms()
        {
            return Ok(token.clone());
        }
        let refresh = self
            .secrets
            .get(&token_key(account_id))
            .map_err(|e| Error::Config(format!("Couldn't read the saved sign-in: {e}")))?
            .ok_or_else(|| {
                Error::Auth(
                    "Sign in to Microsoft again: remove the account and add it back.".into(),
                )
            })?;
        let calendar = self
            .account(account_id)
            .and_then(|a| config(&a))
            .is_ok_and(|c| matches!(c, AccountConfig::Microsoft { calendar: true, .. }));
        let tokens = oauth::refresh(app, &refresh, calendar).await?;
        if let Some(rotated) = &tokens.refresh_token
            && rotated != &refresh
        {
            self.secrets
                .set(&token_key(account_id), rotated)
                .map_err(|e| Error::Config(format!("Couldn't save the sign-in: {e}")))?;
        }
        self.cache_token(account_id, &tokens);
        Ok(tokens.access_token)
    }

    fn cache_token(&self, account_id: i64, tokens: &oauth::Tokens) {
        self.tokens_lock()
            .insert(account_id, (tokens.access_token.clone(), tokens.expires_at));
    }

    fn calendars_lock(&self) -> MutexGuard<'_, HashMap<i64, CalendarStatus>> {
        self.calendars
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn tokens_lock(&self) -> MutexGuard<'_, HashMap<i64, (String, i64)>> {
        self.tokens
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

pub fn config(account: &Account) -> Result<AccountConfig> {
    serde_json::from_str(&account.config).map_err(|e| {
        Error::Config(format!(
            "The settings for {} are damaged: {e}",
            account.email
        ))
    })
}

/// Whether the server has inbox mail older than what's synced, for
/// reaching back. False until the first sync.
pub fn inbox_has_older(account: &Account) -> bool {
    match config(account) {
        Ok(AccountConfig::Imap { .. }) => {
            let state: imap::SyncState = from_json(&account.sync_state);
            state.floor.is_some() && !state.complete
        }
        Ok(AccountConfig::Microsoft { .. }) => {
            let state: graph::SyncState = from_json(&account.sync_state);
            state.delta_link.is_some() && !state.complete
        }
        Err(_) => false,
    }
}

/// Whether the folder has messages older than its synced list. False until
/// its first sync.
pub fn folder_has_older(folder: &Folder) -> bool {
    folder.synced_at.is_some() && !from_json::<Reach>(&folder.sync_state).complete
}

/// What both backends' folder states say about reaching back.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Reach {
    /// Nothing older is left.
    complete: bool,
}

/// Gmail's servers, for app-password accounts.
pub fn gmail_servers() -> (Server, Server) {
    (
        Server::new("imap.gmail.com", 993),
        Server::new("smtp.gmail.com", 465),
    )
}

/// Folder sync failures that mean the account can't be reached fail the
/// round; others are reported and the round goes on.
fn folder_failed(folder: &Folder, e: Error, report: &mut Report) -> Result<()> {
    match e {
        Error::Network { .. } | Error::Auth(_) => Err(e),
        other => {
            report
                .folder_errors
                .push(format!("{}: {other}", folder.name));
            Ok(())
        }
    }
}

fn folder_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::Config("Enter a name for the folder.".into()));
    }
    Ok(name)
}

fn default_label(label: &str, kind: AccountKind, email: &str) -> String {
    let label = label.trim();
    if !label.is_empty() {
        return label.to_owned();
    }
    match kind {
        AccountKind::Gmail => "Gmail".into(),
        AccountKind::Microsoft => "Work".into(),
        AccountKind::Imap => email.split('@').nth(1).unwrap_or(email).to_owned(),
    }
}

fn to_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("settings serialize")
}

fn from_json<T: for<'de> Deserialize<'de> + Default>(text: &str) -> T {
    serde_json::from_str(text).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configs_keep_their_shape() {
        let imap = AccountConfig::Imap {
            username: "me@gmail.com".into(),
            imap: Server::new("imap.gmail.com", 993),
            smtp: Server::new("smtp.gmail.com", 465),
        };
        let json = to_json(&imap);
        assert_eq!(
            json,
            r#"{"type":"imap","username":"me@gmail.com","imap":{"host":"imap.gmail.com","port":993,"security":"tls"},"smtp":{"host":"smtp.gmail.com","port":465,"security":"tls"}}"#
        );
        let microsoft = AccountConfig::Microsoft {
            client_id: "c".into(),
            tenant: "t".into(),
            calendar: false,
        };
        assert_eq!(
            to_json(&microsoft),
            r#"{"type":"microsoft","clientId":"c","tenant":"t"}"#,
            "accounts signed in before the calendar read the same"
        );
        let calendar = AccountConfig::Microsoft {
            client_id: "c".into(),
            tenant: "t".into(),
            calendar: true,
        };
        let with_calendar = to_json(&calendar);
        assert_eq!(
            with_calendar,
            r#"{"type":"microsoft","clientId":"c","tenant":"t","calendar":true}"#
        );
        assert_eq!(
            serde_json::from_str::<AccountConfig>(&with_calendar).unwrap(),
            calendar
        );
        assert_eq!(serde_json::from_str::<AccountConfig>(&json).unwrap(), imap);
    }

    #[test]
    fn older_mail_is_there_until_a_sync_says_otherwise() {
        let imap = AccountConfig::Imap {
            username: "me@x.com".into(),
            imap: Server::new("imap.x.com", 993),
            smtp: Server::new("smtp.x.com", 465),
        };
        let microsoft = AccountConfig::Microsoft {
            client_id: "c".into(),
            tenant: "t".into(),
            calendar: false,
        };
        let account = |config: &AccountConfig, sync_state: &str| Account {
            id: 1,
            kind: AccountKind::Imap,
            email: "me@x.com".into(),
            name: String::new(),
            label: String::new(),
            color: String::new(),
            config: to_json(config),
            sync_state: sync_state.into(),
        };
        // Not synced yet; synced before `complete` was kept; synced in full.
        for (config, unsynced, synced) in [
            (&imap, "{}", r#"{"uidValidity":3,"floor":40}"#),
            (&microsoft, "{}", r#"{"deltaLink":"https://graph/delta"}"#),
        ] {
            assert!(!inbox_has_older(&account(config, unsynced)));
            assert!(inbox_has_older(&account(config, synced)));
            let complete = synced.replace('}', r#","complete":true}"#);
            assert!(!inbox_has_older(&account(config, &complete)));
        }

        let folder = |sync_state: &str, synced_at| Folder {
            id: 1,
            account_id: 1,
            remote_id: "Projects".into(),
            parent_id: None,
            name: "Projects".into(),
            role: None,
            selectable: true,
            sync_state: sync_state.into(),
            synced_at,
        };
        assert!(!folder_has_older(&folder("{}", None)));
        assert!(folder_has_older(&folder(r#"{"uidValidity":3}"#, Some(5))));
        assert!(!folder_has_older(&folder(
            r#"{"limit":300,"complete":true}"#,
            Some(5)
        )));
    }

    #[test]
    fn labels_default_by_kind() {
        assert_eq!(
            default_label(" Personal ", AccountKind::Gmail, "a@gmail.com"),
            "Personal"
        );
        assert_eq!(
            default_label("", AccountKind::Gmail, "a@gmail.com"),
            "Gmail"
        );
        assert_eq!(
            default_label("", AccountKind::Imap, "a@fastmail.com"),
            "fastmail.com"
        );
    }
}
