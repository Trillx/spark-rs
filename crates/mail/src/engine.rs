//! The engine the apps drive: adding accounts, and per account one round of
//! work at a time (send what's due, push local changes, fetch new mail,
//! reach back for older mail, list and sync folders). Callers run rounds
//! for an account one after another; different accounts can run in
//! parallel.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use lettre::message::Mailbox;
use serde::{Deserialize, Serialize};
use spark_store::{
    Account, AccountKind, Folder, FolderRole, Location, NewAccount, NewReminder, Pending, Place,
    Query, Store, now_ms,
};
use tokio::sync::Mutex as AsyncMutex;

use crate::attachment::{self, AttachmentData, InlineImage};
use crate::cache::{AttachmentCache, Flight};
use crate::calendar::{Event, Response};
use crate::draft::{self, Draft, Original, OutgoingAttachment};
use crate::drafts::{self, DraftsReport};
use crate::graph::{self, Graph};
use crate::imap::{self, Server};
use crate::invite::{self, Answer, Invite};
use crate::oauth::{self, App};
use crate::unsubscribe::{self, Unsubscribed};
use crate::{Error, OLDER_STEP, Result, Service, search, smtp};

/// How long a local change waits before it's pushed: longer than the UI's
/// undo window, so an undone change never reaches the server.
pub const SETTLE_MS: i64 = 6_000;
/// A send that failed on the network is retried after this long.
const SEND_RETRY_MS: i64 = 60_000;
/// How many waiting senders a round asks the Sent folder about, at most.
const SENT_CHECKS: usize = 20;
/// A waiting sender the Sent folder didn't have is asked about again after
/// this long (the user may have written to them elsewhere since), or when
/// they write again.
const SENT_RECHECK_MS: i64 = 60 * 60_000;

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
    /// Cached messages whose reply headers changed.
    pub headers: usize,
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
    /// Senders waiting in the Gatekeeper that the Sent folder showed the
    /// user had written to: they're accepted, so their mail moved.
    pub accepted: usize,
}

impl Report {
    /// Whether the message list needs reloading.
    pub fn changed(&self) -> bool {
        self.new
            + self.gone
            + self.older
            + self.folder_changes
            + self.drafts
            + self.headers
            + self.accepted
            > 0
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
    /// The editable draft recovered after a permanent failure.
    pub draft_id: Option<i64>,
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

/// Auto-download leaves alone an email whose attachments add up to more.
pub const PREFETCH_MAX_BYTES: u64 = 30 * 1024 * 1024;

pub struct Engine {
    store: Arc<Mutex<Store>>,
    secrets: Arc<dyn Secrets>,
    /// Microsoft access tokens and their expiry (Unix ms), in memory only.
    tokens: Mutex<HashMap<i64, (String, i64)>>,
    /// How each account's last calendar refresh went.
    calendars: Mutex<HashMap<i64, CalendarStatus>>,
    /// Per account, taken by rounds, deleting for good and emptying
    /// folders, so they change the server one at a time: a round pushing a
    /// move can't land between an empty's look at what's pending and its
    /// expunge.
    turns: Mutex<HashMap<i64, Arc<AsyncMutex<()>>>>,
    /// Senders waiting in an account's Gatekeeper that its Sent folder
    /// didn't have, by account and lowercased address, with when it was asked.
    not_written_to: Mutex<HashMap<(i64, String), i64>>,
    /// Downloaded attachments kept on this computer, if they're kept.
    files: Option<Arc<AttachmentCache>>,
    /// Where Graph is: `graph::GRAPH`, or a stand-in's address in tests.
    graph_base: String,
}

impl Engine {
    pub fn new(store: Arc<Mutex<Store>>, secrets: Arc<dyn Secrets>) -> Self {
        Self {
            store,
            secrets,
            tokens: Mutex::new(HashMap::new()),
            calendars: Mutex::new(HashMap::new()),
            turns: Mutex::new(HashMap::new()),
            not_written_to: Mutex::new(HashMap::new()),
            files: None,
            graph_base: graph::GRAPH.to_owned(),
        }
    }

    /// Keeps downloaded attachments in `dir` (see `cache`), taking out
    /// those of emails that went while the app was closed.
    pub fn with_attachment_cache(mut self, dir: std::path::PathBuf) -> Self {
        self.files = Some(Arc::new(AttachmentCache::new(dir)));
        self.sweep_files();
        if let Some(files) = &self.files {
            files.prune();
        }
        self
    }

    /// Takes the kept attachments of emails and accounts the store no
    /// longer has off this computer.
    pub fn sweep_files(&self) {
        let Some(files) = &self.files else {
            return;
        };
        let keep = {
            let store = self.store();
            let Ok(accounts) = store.accounts() else {
                return;
            };
            let mut keep = HashMap::new();
            for account in accounts {
                let Ok(keys) = store.files_keys(account.id) else {
                    return;
                };
                let names = keys.into_iter().map(|(id, key)| format!("{id}-{key}"));
                keep.insert(account.id, names.collect());
            }
            keep
        };
        files.sweep(&keep);
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
        // A certificate to trust is asked about now, not at the first send.
        smtp::check_certificate(&smtp).await?;

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

    /// Trusts the certificate one of an IMAP account's servers shows, by its
    /// fingerprint (`Untrusted::fingerprint`), where the system doesn't.
    pub fn trust_certificate(
        &self,
        account_id: i64,
        service: Service,
        fingerprint: &str,
    ) -> Result<()> {
        let account = self.account(account_id)?;
        let AccountConfig::Imap {
            username,
            mut imap,
            mut smtp,
        } = config(&account)?
        else {
            return Err(Error::Config(
                "Only IMAP accounts have certificates to trust.".into(),
            ));
        };
        let server = match service {
            Service::Imap => &mut imap,
            Service::Smtp => &mut smtp,
        };
        server.trusted_cert = Some(fingerprint.to_owned());
        let config = AccountConfig::Imap {
            username,
            imap,
            smtp,
        };
        self.store()
            .set_account_config(account_id, &to_json(&config))?;
        Ok(())
    }

    /// Removes the account, its mail and its saved password or sign-in.
    pub fn remove_account(&self, account_id: i64) -> Result<()> {
        self.store().remove_account(account_id)?;
        // After the store: a download finishing now finds the account gone.
        if let Some(files) = &self.files {
            files.forget_account(account_id);
        }
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
        let turn = self.turn(account_id);
        let _turn = turn.lock().await;
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
                report.pushed = imap::push(&mut session, &self.store, &mut state, pending).await?;
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
                    report.headers += sync.headers;
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
                let waiting = self.waiting_senders(account_id, round, &report.arrived)?;
                if !waiting.is_empty()
                    && let Some(sent) = self.sent_folder(account_id)?
                    // A failed check leaves them to the next round; it doesn't fail this one.
                    && let Ok(found) = imap::written_to(&mut session, &sent, &waiting).await
                {
                    report.accepted += self.written_to(account_id, &waiting, &found)?;
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
                    report.headers += sync.headers;
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
                let waiting = self.waiting_senders(account_id, round, &report.arrived)?;
                if !waiting.is_empty()
                    && let Ok(found) = graph.written_to(&waiting).await
                {
                    report.accepted += self.written_to(account_id, &waiting, &found)?;
                }
            }
        }
        Ok(report)
    }

    /// Senders waiting in any account's Gatekeeper to ask this account's
    /// Sent folder about, on rounds that fetch: the user may have written to
    /// them elsewhere (Spark reads Sent for the same reason), and a decision
    /// counts for every account. Those asked here lately without luck wait,
    /// unless they've written again.
    fn waiting_senders(
        &self,
        account_id: i64,
        round: &Round,
        arrived: &[i64],
    ) -> Result<Vec<String>> {
        if !round.fetch {
            return Ok(Vec::new());
        }
        let store = self.store();
        let mut asked = self
            .not_written_to
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for id in arrived {
            if let Some(m) = store.message(*id)? {
                let address = m.from_address.to_lowercase();
                asked.retain(|(_, a), _| *a != address);
            }
        }
        let now = now_ms();
        let mut senders: Vec<String> = Vec::new();
        for address in store.screened_senders()? {
            let key = (account_id, address.to_lowercase());
            let lately = asked.get(&key).is_some_and(|at| now - at < SENT_RECHECK_MS);
            if !lately && !senders.iter().any(|s| s.eq_ignore_ascii_case(&address)) {
                senders.push(address);
            }
            if senders.len() == SENT_CHECKS {
                break;
            }
        }
        Ok(senders)
    }

    /// The Sent folder had mail to `found` of the `asked`: they're accepted,
    /// unless decided about meanwhile. Returns how many were.
    fn written_to(&self, account_id: i64, asked: &[String], found: &[String]) -> Result<usize> {
        let now = now_ms();
        {
            let mut not = self
                .not_written_to
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for address in asked {
                let key = (account_id, address.to_lowercase());
                if found.iter().any(|f| f.eq_ignore_ascii_case(address)) {
                    not.remove(&key);
                } else {
                    not.insert(key, now);
                }
            }
        }
        let store = self.store();
        let mut accepted = 0;
        for address in found {
            accepted += usize::from(store.accept_sender(address, now)?);
        }
        Ok(accepted)
    }

    /// The account's Sent folder, as an IMAP mailbox name.
    fn sent_folder(&self, account_id: i64) -> Result<Option<String>> {
        Ok(self
            .store()
            .folders()?
            .into_iter()
            .find(|f| f.account_id == account_id && f.role == Some(FolderRole::Sent))
            .map(|f| f.remote_id))
    }

    /// Puts a search (in the search field's syntax) to the account's server
    /// and stores the mail it finds that isn't stored yet, so the store's
    /// search has it (see the `search` module). Returns how many messages
    /// were new. It runs beside the account's rounds, on a connection of
    /// its own.
    pub async fn search_server(&self, account_id: i64, query: &str) -> Result<usize> {
        let query = Query::parse(query);
        if !search::worth_asking(&query) {
            return Ok(0);
        }
        let account = self.account(account_id)?;
        match config(&account)? {
            AccountConfig::Imap {
                username,
                imap: server,
                ..
            } => {
                let state: imap::SyncState = from_json(&account.sync_state);
                let mut session =
                    imap::login(&server, &username, &self.password(account_id)?).await?;
                let found =
                    imap::search_server(&mut session, &self.store, &account, &state, &query).await;
                let _ = session.logout().await;
                found
            }
            AccountConfig::Microsoft {
                client_id, tenant, ..
            } => {
                let state: graph::SyncState = from_json(&account.sync_state);
                self.graph(account_id, &client_id, &tenant)
                    .await?
                    .search_server(&self.store, account_id, &state, &query)
                    .await
            }
        }
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

    /// Deletes stored messages for good, on their servers and here, wherever
    /// the servers have them now. Returns how many went.
    pub async fn delete_forever(&self, ids: &[i64]) -> Result<usize> {
        let mut by_account: HashMap<i64, Vec<i64>> = HashMap::new();
        for &id in ids {
            if let Some(at) = self.store().location(id)? {
                by_account.entry(at.account_id).or_default().push(id);
            }
        }
        let mut removed = 0;
        for (account_id, ids) in by_account {
            let turn = self.turn(account_id);
            let _turn = turn.lock().await;
            let account = self.account(account_id)?;
            removed += match config(&account)? {
                AccountConfig::Imap {
                    username,
                    imap: server,
                    ..
                } => {
                    let spam = self.role_folder(account_id, FolderRole::Spam)?;
                    let mut state: imap::SyncState = from_json(&account.sync_state);
                    let mut session =
                        imap::login(&server, &username, &self.password(account_id)?).await?;
                    let deleted = async {
                        imap::find_folders(&mut session, &mut state).await?;
                        let spam = spam.as_ref().map(|f| f.remote_id.as_str());
                        imap::delete_forever(&mut session, &self.store, &state, spam, &ids).await
                    }
                    .await;
                    let _ = session.logout().await;
                    deleted?
                }
                AccountConfig::Microsoft {
                    client_id, tenant, ..
                } => {
                    let graph = self.graph(account_id, &client_id, &tenant).await?;
                    let mut removed = 0;
                    for id in ids {
                        let remote = self.store().location(id)?.and_then(|at| at.remote_id);
                        if let Some(remote) = remote {
                            graph.delete_forever(&remote).await?;
                        }
                        removed += self.store().remove_messages(&[id])?;
                    }
                    removed
                }
            };
        }
        // Their kept attachments go with them.
        self.sweep_files();
        Ok(removed)
    }

    /// Deletes everything in a folder for good, on the server and here.
    /// Changes still on their way into or out of it are pushed first, so
    /// what goes is what the folder shows. On Gmail only Trash and Spam
    /// can be emptied: expunging a label only takes the label off. Returns
    /// how many emails the server deleted.
    pub async fn empty_folder(&self, folder_id: i64) -> Result<usize> {
        let folder = self.folder(folder_id)?;
        let account = self.account(folder.account_id)?;
        let deletes = matches!(folder.role, Some(FolderRole::Trash | FolderRole::Spam));
        if account.kind == AccountKind::Gmail && !deletes {
            return Err(Error::Config(
                "On Gmail, only Trash and Spam can be emptied.".into(),
            ));
        }
        // Pending changes are read with the turn taken: none is half pushed.
        let turn = self.turn(account.id);
        let _turn = turn.lock().await;
        let pending: Vec<Pending> = self
            .store()
            .pending(account.id, now_ms(), 0)?
            .into_iter()
            .filter(|p| moves_through(p, &folder))
            .collect();
        let emptied = match config(&account)? {
            AccountConfig::Imap {
                username,
                imap: server,
                ..
            } => {
                let mut state: imap::SyncState = from_json(&account.sync_state);
                let mut session =
                    imap::login(&server, &username, &self.password(account.id)?).await?;
                let emptied = async {
                    imap::find_folders(&mut session, &mut state).await?;
                    imap::push(&mut session, &self.store, &mut state, pending).await?;
                    imap::empty_folder(&mut session, &folder.remote_id).await
                }
                .await;
                let _ = session.logout().await;
                emptied? as usize
            }
            AccountConfig::Microsoft {
                client_id, tenant, ..
            } => {
                let graph = self.graph(account.id, &client_id, &tenant).await?;
                graph.push(&self.store, pending).await?;
                graph.empty_folder(&folder.remote_id).await?
            }
        };
        self.store().forget_emptied(folder_id)?;
        self.sweep_files();
        Ok(emptied)
    }

    /// The account's turn at changing its server (`turns`).
    fn turn(&self, account_id: i64) -> Arc<AsyncMutex<()>> {
        self.turns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(account_id)
            .or_default()
            .clone()
    }

    /// The account's folder with this role, if it has one.
    fn role_folder(&self, account_id: i64, role: FolderRole) -> Result<Option<Folder>> {
        Ok(self
            .store()
            .folders()?
            .into_iter()
            .find(|f| f.account_id == account_id && f.role == Some(role)))
    }

    fn folder(&self, folder_id: i64) -> Result<Folder> {
        self.store()
            .folder(folder_id)?
            .ok_or_else(|| Error::Config("That folder is gone.".into()))
    }

    async fn graph(&self, account_id: i64, client_id: &str, tenant: &str) -> Result<Graph> {
        Ok(Graph::with_base(
            &self.graph_base,
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
            let recipients = parsed.as_ref().map(drafts::sent_to).unwrap_or_default();
            let result = match &parsed {
                Ok(draft) => self.send_draft(account_id, id, draft).await,
                Err(e) => Err(Error::Config(format!(
                    "The queued message is unreadable: {e}"
                ))),
            };
            let mut draft_id = None;
            let result = match result {
                Ok(message_id) => {
                    // The saved draft it came from is done with, here and on the server.
                    if let Some(saved) = saved_draft {
                        self.store().discard_draft(saved)?;
                    }
                    self.store().finish_send(id)?;
                    if let Ok(draft) = &parsed
                        && let Some(remind) = &draft.remind
                    {
                        let to = [&draft.to, &draft.cc, &draft.bcc]
                            .into_iter()
                            .filter(|f| !f.trim().is_empty())
                            .map(|f| f.trim())
                            .collect::<Vec<_>>()
                            .join(", ");
                        self.store().add_reminder(&NewReminder {
                            account_id,
                            message_id: message_id.as_deref(),
                            reply_to: draft.reply_to,
                            subject: draft.subject.trim(),
                            to: &to,
                            sent_at: now_ms(),
                            remind_at: remind.at,
                            if_no_reply: remind.if_no_reply,
                        })?;
                    }
                    // Delivery is complete even if learning its recipients fails.
                    let _ = self.store().record_sent(&recipients, now_ms());
                    Ok(true)
                }
                // Offline or the server is busy: keep it queued.
                Err(Error::Network { .. }) => {
                    self.store().retry_send(id, now_ms() + SEND_RETRY_MS)?;
                    Ok(false)
                }
                Err(e) => {
                    if let Ok(draft) = parsed {
                        let mut store = self.store();
                        let account = store
                            .account(account_id)?
                            .ok_or_else(|| Error::Config("That account was removed.".into()))?;
                        let files = store.outbox_files(id)?;
                        draft_id = Some(drafts::recover_failed_send(
                            &mut store,
                            &account,
                            &draft,
                            &files,
                            now_ms(),
                            id,
                        )?);
                    }
                    Err(e)
                }
            };
            outcomes.push(SendOutcome {
                id,
                draft_id,
                result,
            });
        }
        Ok(outcomes)
    }

    /// Takes a scheduled send out of the queue into Drafts, exactly as it was
    /// queued, its files too (in one step, so nothing is lost), to edit or to
    /// keep; `keep_schedule` keeps its Send Later time on the draft. Returns
    /// the draft, or `None` once it's sending or gone.
    pub fn unschedule(&self, outbox_id: i64, keep_schedule: bool) -> Result<Option<i64>> {
        let mut store = self.store();
        let Some((account_id, json)) = store.queued_send(outbox_id)? else {
            return Ok(None);
        };
        let mut draft: Draft = serde_json::from_str(&json)
            .map_err(|e| Error::Config(format!("The queued message is unreadable: {e}")))?;
        if !keep_schedule {
            draft.send_later = None;
        }
        let account = store
            .account(account_id)?
            .ok_or_else(|| Error::Config("That account was removed.".into()))?;
        let files = store.outbox_files(outbox_id)?;
        let id =
            drafts::recover_failed_send(&mut store, &account, &draft, &files, now_ms(), outbox_id)?;
        Ok(Some(id))
    }

    /// Sends a queued draft. Returns its Message-ID, for reminders.
    async fn send_draft(
        &self,
        account_id: i64,
        outbox_id: i64,
        draft: &Draft,
    ) -> Result<Option<String>> {
        let account = self.account(account_id)?;
        let original = draft
            .reply_to
            .map(|id| self.original(id))
            .transpose()?
            .flatten()
            .or_else(|| drafts::original_from_headers(draft));
        let files = self.outgoing_files(outbox_id, draft).await?;
        let message_id = match config(&account)? {
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
                let message_id = message.headers().get_raw("Message-ID").map(str::to_owned);
                smtp::send(&server, &username, &self.password(account_id)?, message).await?;
                message_id
            }
            AccountConfig::Microsoft {
                client_id, tenant, ..
            } => {
                let token = self
                    .access_token(account_id, &App::new(&client_id, &tenant)?)
                    .await?;
                let remote = original.as_ref().and_then(|(_, remote)| remote.as_deref());
                Graph::new(&token).send_draft(draft, remote, &files).await?
            }
        };
        // People you write to skip the Gatekeeper.
        let store = self.store();
        for field in [&draft.to, &draft.cc, &draft.bcc] {
            for m in draft::recipients(field).unwrap_or_default() {
                store.accept_sender(m.email.as_ref(), now_ms())?;
            }
        }
        Ok(message_id)
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
            let listed = graph.calendars(&account.email).await?;
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
    /// forwarded ones fetched again from the server, each email's in one go
    /// (`attachments`).
    async fn outgoing_files(&self, outbox_id: i64, draft: &Draft) -> Result<Vec<AttachmentData>> {
        let mut queued = self.store().outbox_files(outbox_id)?.into_iter();
        let mut files = Vec::with_capacity(draft.attachments.len());
        // Per email forwarded from, its attachments and their places.
        let mut forwarded: Vec<(i64, Vec<usize>, Vec<usize>)> = Vec::new();
        for a in &draft.attachments {
            match a {
                OutgoingAttachment::File { name, .. } => {
                    let f = queued.next().ok_or_else(|| {
                        Error::Config(format!("{name} is missing from the outbox."))
                    })?;
                    files.push(Some(AttachmentData {
                        name: f.name,
                        mime: f.mime,
                        bytes: f.data,
                    }));
                }
                OutgoingAttachment::Message { message, index, .. } => {
                    match forwarded.iter_mut().find(|(m, ..)| m == message) {
                        Some((_, indices, places)) => {
                            indices.push(*index);
                            places.push(files.len());
                        }
                        None => forwarded.push((*message, vec![*index], vec![files.len()])),
                    }
                    files.push(None);
                }
            }
        }
        for (message, indices, places) in forwarded {
            let fetched = self.attachments(message, &indices).await?;
            for (place, file) in places.into_iter().zip(fetched) {
                files[place] = Some(file);
            }
        }
        Ok(files.into_iter().flatten().collect())
    }

    /// Attachment `index` of a stored message (see `attachments`).
    pub async fn attachment(&self, message_id: i64, index: usize) -> Result<AttachmentData> {
        self.attachments(message_id, &[index])
            .await?
            .pop()
            .ok_or_else(not_attached)
    }

    /// Attachments `indices` of a stored message, in that order: the ones
    /// kept here, and the rest downloaded from its server in one go (and
    /// kept). For IMAP that's one session and one download of the email
    /// however many are missing; for Microsoft, a few at a time.
    pub async fn attachments(
        &self,
        message_id: i64,
        indices: &[usize],
    ) -> Result<Vec<AttachmentData>> {
        let email = self.email_files(message_id)?;
        if indices.iter().any(|&i| i >= email.attachments.len()) {
            return Err(not_attached());
        }
        let mut files = self.kept(&email, indices).await?;
        if !missing(&files, indices).is_empty() {
            // One download at a time: what another brought while this one
            // waited is kept by then.
            let _turn = self.turn_at(&email, FILES).await;
            let came = self.kept(&email, &missing(&files, indices)).await?;
            fill(&mut files, came);
            let missing = missing(&files, indices);
            if !missing.is_empty() {
                let account = self.account(email.at.account_id)?;
                let fetched = self
                    .download(&account, config(&account)?, &email, &missing)
                    .await?;
                fill(&mut files, fetched);
            }
        }
        indices
            .iter()
            .enumerate()
            .map(|(n, &i)| {
                // The same one asked for twice is handed over twice.
                let file = if indices[n + 1..].contains(&i) {
                    files[i].clone()
                } else {
                    files[i].take()
                };
                file.ok_or_else(|| {
                    Error::Server(format!(
                        "{} wasn't in the email on the server.",
                        email.attachments[i].name
                    ))
                })
            })
            .collect()
    }

    /// Spark's Auto-download: fetches the attachments of a stored message
    /// that aren't kept yet, unless there's no cache or they add up to more
    /// than `PREFETCH_MAX_BYTES`. Returns how many it fetched.
    pub async fn prefetch_attachments(&self, message_id: i64) -> Result<usize> {
        let Some(files) = &self.files else {
            return Ok(0);
        };
        let email = self.email_files(message_id)?;
        let missing = || -> Vec<usize> {
            (0..email.attachments.len())
                .filter(|&i| !files.has(email.at.account_id, &email.key, i))
                .collect()
        };
        let size: u64 = email.attachments.iter().map(|a| a.size).sum();
        if size > PREFETCH_MAX_BYTES || missing().is_empty() {
            return Ok(0);
        }
        let _turn = self.turn_at(&email, FILES).await;
        let missing = missing();
        if missing.is_empty() {
            return Ok(0);
        }
        let account = self.account(email.at.account_id)?;
        let fetched = self
            .download(&account, config(&account)?, &email, &missing)
            .await?;
        Ok(missing.iter().filter(|&&i| fetched[i].is_some()).count())
    }

    /// What a stored message's files are found and kept by. Read in one go,
    /// so its UIDVALIDITY is the one its location was stored with.
    fn email_files(&self, message_id: i64) -> Result<EmailFiles> {
        let store = self.store();
        let gone = || Error::Config("That email is no longer here.".into());
        let at = store.location(message_id)?.ok_or_else(gone)?;
        let content = store.content(message_id)?.ok_or_else(gone)?;
        let validity = stored_validity(&store, &at)?;
        let mut fresh = [0u8; 12];
        getrandom::fill(&mut fresh).map_err(|e| Error::Config(e.to_string()))?;
        let fresh: String = fresh.iter().map(|b| format!("{b:02x}")).collect();
        let key = store.files_key(message_id, &fresh)?.ok_or_else(gone)?;
        Ok(EmailFiles {
            at,
            validity,
            attachments: content.attachments,
            images: content
                .html
                .as_deref()
                .map(attachment::cid_refs)
                .unwrap_or_default(),
            key: format!("{message_id}-{key}"),
            draft: store.is_draft(message_id)?,
        })
    }

    /// Waits for the turn to look up and download `part` of an email's
    /// files (`AttachmentCache::flight`); there's none to wait for without
    /// a cache.
    async fn turn_at(&self, email: &EmailFiles, part: &str) -> Option<Flight<'_>> {
        let files = self.files.as_ref()?;
        let name = format!("{}/{}/{part}", email.at.account_id, email.key);
        Some(files.flight(name).await)
    }

    /// The kept copies of attachments `indices` of an email, read on a
    /// blocking thread: by index as the email lists them, `None` for one
    /// that isn't kept (or wasn't asked for).
    async fn kept(
        &self,
        email: &EmailFiles,
        indices: &[usize],
    ) -> Result<Vec<Option<AttachmentData>>> {
        let Some(files) = self.files.clone() else {
            return Ok(vec![None; email.attachments.len()]);
        };
        let (account, key) = (email.at.account_id, email.key.clone());
        let listed: Vec<(bool, String, String)> = email
            .attachments
            .iter()
            .enumerate()
            .map(|(i, a)| (indices.contains(&i), a.name.clone(), a.mime.clone()))
            .collect();
        blocking(move || {
            listed
                .iter()
                .enumerate()
                .map(|(i, (asked, name, mime))| {
                    asked
                        .then(|| files.get(account, &key, i, name, mime))
                        .flatten()
                })
                .collect()
        })
        .await
    }

    /// The images an email embeds, read on a blocking thread, if they're
    /// kept. A draft's never are.
    async fn kept_images(&self, email: &EmailFiles) -> Result<Option<Vec<InlineImage>>> {
        let Some(files) = self.files.clone().filter(|_| !email.draft) else {
            return Ok(None);
        };
        let (account, key, cids) = (email.at.account_id, email.key.clone(), email.images.clone());
        blocking(move || files.get_images(account, &key, &cids)).await
    }

    /// Downloads attachments `missing` of a stored message from its server,
    /// and keeps them: for IMAP the whole email, once, whose other files and
    /// embedded images are kept too; for Microsoft just those, a few at a
    /// time. By index as the email lists them, `None` for one that isn't
    /// there (or wasn't downloaded).
    async fn download(
        &self,
        account: &Account,
        config: AccountConfig,
        email: &EmailFiles,
        missing: &[usize],
    ) -> Result<Vec<Option<AttachmentData>>> {
        match config {
            AccountConfig::Imap {
                username,
                imap: server,
                ..
            } => Ok(self
                .download_imap(account, &server, &username, email)
                .await?
                .0),
            AccountConfig::Microsoft {
                client_id, tenant, ..
            } => {
                let remote = email.at.remote_id.clone().ok_or(Error::Gone)?;
                let asked: Vec<(usize, spark_store::Attachment)> = missing
                    .iter()
                    .map(|&i| (i, email.attachments[i].clone()))
                    .collect();
                let fetched = self
                    .graph(account.id, &client_id, &tenant)
                    .await?
                    .attachment_files(&remote, &asked)
                    .await?;
                let mut files = vec![None; email.attachments.len()];
                for (&i, file) in missing.iter().zip(fetched) {
                    files[i] = Some(file);
                }
                Ok(self.keep(email, files, None).await?.0)
            }
        }
    }

    /// Downloads a stored IMAP email once and takes all its files out of it
    /// with one parse, on a blocking thread: its attachments, by index as
    /// the store lists them (`None` for one that isn't there), and the
    /// images its HTML embeds. They're kept, unless the email had to be
    /// found by Message-ID because its mailbox was renumbered
    /// (`imap::fetch_raw_checked`).
    async fn download_imap(
        &self,
        account: &Account,
        server: &Server,
        username: &str,
        email: &EmailFiles,
    ) -> Result<(Vec<Option<AttachmentData>>, Vec<InlineImage>)> {
        let (raw, trusted) = self
            .fetch_raw(account, server, username, &email.at, email.validity)
            .await?;
        let names: Vec<String> = email.attachments.iter().map(|a| a.name.clone()).collect();
        let cids = email.images.clone();
        let (files, images) = blocking(move || {
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            attachment::extract_files(&raw, &names, &cids)
        })
        .await?;
        if !trusted {
            return Ok((files, images));
        }
        let (files, images) = self.keep(email, files, Some(images)).await?;
        Ok((files, images.unwrap_or_default()))
    }

    /// Keeps what was downloaded of an email, on a blocking thread, unless
    /// its account went meanwhile; then makes room. `files` are its
    /// attachments by index, `images` those its HTML embeds if they were
    /// fetched (not kept for a draft). Hands them back.
    async fn keep(
        &self,
        email: &EmailFiles,
        files: Vec<Option<AttachmentData>>,
        images: Option<Vec<InlineImage>>,
    ) -> Result<(Vec<Option<AttachmentData>>, Option<Vec<InlineImage>>)> {
        let Some(cache) = self.files.clone() else {
            return Ok((files, images));
        };
        let store = self.store.clone();
        let (account, key, cids) = (email.at.account_id, email.key.clone(), email.images.clone());
        let draft = email.draft;
        blocking(move || {
            {
                let _held = cache.lock();
                let store = store
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if !matches!(store.account(account), Ok(Some(_))) {
                    return (files, images);
                }
                drop(store);
                for (index, file) in files.iter().enumerate() {
                    if let Some(file) = file {
                        cache.put(account, &key, index, &file.bytes);
                    }
                }
                if let Some(images) = &images
                    && !cids.is_empty()
                    && !draft
                {
                    cache.put_images(account, &key, &cids, images);
                }
            }
            cache.prune();
            (files, images)
        })
        .await
    }

    /// A stored message as its server has it, MIME and all: what Save as
    /// .eml writes.
    pub async fn raw_message(&self, message_id: i64) -> Result<Vec<u8>> {
        let (at, validity) = self.locate(message_id)?;
        let account = self.account(at.account_id)?;
        match config(&account)? {
            AccountConfig::Imap {
                username,
                imap: server,
                ..
            } => Ok(self
                .fetch_raw(&account, &server, &username, &at, validity)
                .await?
                .0),
            AccountConfig::Microsoft {
                client_id, tenant, ..
            } => {
                let remote = at.remote_id.ok_or(Error::Gone)?;
                let token = self
                    .access_token(account.id, &App::new(&client_id, &tenant)?)
                    .await?;
                Graph::new(&token).raw(&remote).await
            }
        }
    }

    /// Unsubscribes from the mailing list a stored message came from, as its
    /// `List-Unsubscribe` header offers (see `unsubscribe`). The header is
    /// read from the server, so mail stored before this works too.
    pub async fn unsubscribe(&self, message_id: i64) -> Result<Unsubscribed> {
        let (at, validity) = self.locate(message_id)?;
        let account = self.account(at.account_id)?;
        let (list, post) = match config(&account)? {
            AccountConfig::Imap {
                username,
                imap: server,
                ..
            } => {
                let (raw, _) = self
                    .fetch_raw(&account, &server, &username, &at, validity)
                    .await?;
                let parsed = mail_parser::MessageParser::default()
                    .parse(&raw)
                    .ok_or_else(|| {
                        Error::Server("The email on the server is unreadable.".into())
                    })?;
                let header = |name: &str| parsed.header_raw(name).map(str::to_owned);
                (header("List-Unsubscribe"), header("List-Unsubscribe-Post"))
            }
            AccountConfig::Microsoft {
                client_id, tenant, ..
            } => {
                let remote = at.remote_id.ok_or(Error::Gone)?;
                let headers = self
                    .graph(account.id, &client_id, &tenant)
                    .await?
                    .headers(&remote)
                    .await?;
                let header = |name: &str| {
                    headers
                        .iter()
                        .find(|(n, _)| n.eq_ignore_ascii_case(name))
                        .map(|(_, v)| v.clone())
                };
                (header("List-Unsubscribe"), header("List-Unsubscribe-Post"))
            }
        };
        let list = list.ok_or_else(|| {
            Error::Config("This email doesn't say how to unsubscribe from it.".into())
        })?;
        unsubscribe::unsubscribe(unsubscribe::options(&list, post.as_deref())).await
    }

    /// The images a stored message's HTML embeds with `cid:` URLs: the ones
    /// kept here, else from its server (and kept). A draft's always come from
    /// its server, as they can change there. Empty, without asking the
    /// server, when it embeds none.
    pub async fn inline_images(&self, message_id: i64) -> Result<Vec<InlineImage>> {
        let email = self.email_files(message_id)?;
        if email.images.is_empty() {
            return Ok(Vec::new());
        }
        if let Some(kept) = self.kept_images(&email).await? {
            return Ok(kept);
        }
        let account = self.account(email.at.account_id)?;
        let config = config(&account)?;
        // An IMAP email's images come with its attachments, in one download;
        // Microsoft's come apart, so they needn't wait for those.
        let part = match config {
            AccountConfig::Imap { .. } => FILES,
            AccountConfig::Microsoft { .. } => IMAGES,
        };
        let _turn = self.turn_at(&email, part).await;
        if let Some(kept) = self.kept_images(&email).await? {
            return Ok(kept);
        }
        match config {
            AccountConfig::Imap {
                username,
                imap: server,
                ..
            } => Ok(self
                .download_imap(&account, &server, &username, &email)
                .await?
                .1),
            AccountConfig::Microsoft {
                client_id, tenant, ..
            } => {
                let remote = email.at.remote_id.clone().ok_or(Error::Gone)?;
                let images = self
                    .graph(account.id, &client_id, &tenant)
                    .await?
                    .inline_images(&remote, &email.images)
                    .await?;
                let (_, images) = self.keep(&email, Vec::new(), Some(images)).await?;
                Ok(images.unwrap_or_default())
            }
        }
    }

    /// Where a stored message is, and the UIDVALIDITY its UID was stored
    /// under (`stored_validity`), read in one go.
    fn locate(&self, message_id: i64) -> Result<(Location, Option<u32>)> {
        let store = self.store();
        let at = store
            .location(message_id)?
            .ok_or_else(|| Error::Config("That email is no longer here.".into()))?;
        let validity = stored_validity(&store, &at)?;
        Ok((at, validity))
    }

    /// A stored message as the IMAP server has it, wherever it is now, and
    /// whether it's safe to keep what's in it (`imap::fetch_raw_checked`).
    /// `validity` is the UIDVALIDITY its UID was stored under.
    async fn fetch_raw(
        &self,
        account: &Account,
        server: &Server,
        username: &str,
        at: &Location,
        validity: Option<u32>,
    ) -> Result<(Vec<u8>, bool)> {
        let password = self.password(account.id)?;
        let state: imap::SyncState = from_json(&account.sync_state);
        let mut session = imap::login(server, username, &password).await?;
        let raw = imap::fetch_raw_checked(&mut session, &state, at, validity).await;
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
                html: content.html,
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
/// Whether a pending change moves a message into or out of `folder`.
fn moves_through(p: &Pending, folder: &Folder) -> bool {
    let at = |place: Place, filed_in: Option<i64>| match folder.role {
        Some(FolderRole::Trash) => place == Place::Trash,
        Some(FolderRole::Archive) => place == Place::Archive,
        Some(FolderRole::Blocked) => place == Place::Blocked,
        _ => place == Place::Folder && filed_in == Some(folder.id),
    };
    at(p.place, p.folder.as_ref().map(|(id, _)| *id)) || at(p.remote_place, p.remote_folder)
}

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

/// The parts of an email's files that are downloaded apart (`turn_at`).
const FILES: &str = "files";
const IMAGES: &str = "images";

/// What a stored message's files are found and kept by (`email_files`).
struct EmailFiles {
    at: Location,
    /// The UIDVALIDITY its UID was stored under, if it's known.
    validity: Option<u32>,
    attachments: Vec<spark_store::Attachment>,
    /// The Content-IDs of the images its HTML embeds (`cid_refs`).
    images: Vec<String>,
    /// Its name in the cache: its id and `files_key`, as a stored id can be
    /// another email's once the first is gone.
    key: String,
    /// It's a draft, which can change on the server keeping its id and
    /// Content-IDs: its images aren't kept, but come from there each time.
    draft: bool,
}

/// The UIDVALIDITY a stored IMAP message's UID was given under: its
/// mailbox's as last synced, if it has a UID and that's known.
fn stored_validity(store: &Store, at: &Location) -> Result<Option<u32>> {
    let (Some(mailbox), Some(_)) = (&at.mailbox, at.uid) else {
        return Ok(None);
    };
    if mailbox.eq_ignore_ascii_case(imap::INBOX) {
        let account = store.account(at.account_id)?;
        return Ok(account.and_then(|a| from_json::<imap::SyncState>(&a.sync_state).uid_validity));
    }
    Ok(store
        .folders()?
        .into_iter()
        .find(|f| f.account_id == at.account_id && &f.remote_id == mailbox)
        .and_then(|f| from_json::<imap::FolderState>(&f.sync_state).uid_validity))
}

/// Attachments `indices` that `files` (by index) doesn't have, each once.
fn missing(files: &[Option<AttachmentData>], indices: &[usize]) -> Vec<usize> {
    (0..files.len())
        .filter(|&i| files[i].is_none() && indices.contains(&i))
        .collect()
}

/// Fills in what `files` doesn't have from `found`, both by index.
fn fill(files: &mut [Option<AttachmentData>], found: Vec<Option<AttachmentData>>) {
    for (file, found) in files.iter_mut().zip(found) {
        if file.is_none() {
            *file = found;
        }
    }
}

fn not_attached() -> Error {
    Error::Config("That attachment isn't part of the email.".into())
}

/// Runs `work` on a blocking thread, so parsing a big email and file work
/// don't hold up the async ones. A panic in it carries on here.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    match tokio::task::spawn_blocking(work).await {
        Ok(done) => Ok(done),
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(_) => Err(Error::Server("Reading the email was stopped.".into())),
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
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering::SeqCst};

    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use spark_store::{Added, Attachment, FolderRole, Incoming, OutboxFile, RemoteFolder};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{TcpListener, TcpStream};

    struct TestSecrets;

    impl Secrets for TestSecrets {
        fn get(&self, _: &str) -> Result<Option<String>, String> {
            Ok(Some("test-password".into()))
        }

        fn set(&self, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
    }

    /// One SMTP delivery, rejecting its recipients or accepting its DATA.
    async fn smtp_delivery(code: u16) -> (Server, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(socket);
            stream
                .get_mut()
                .write_all(b"220 test ESMTP\r\n")
                .await
                .unwrap();
            let mut data = false;
            loop {
                let mut line = String::new();
                if stream.read_line(&mut line).await.unwrap() == 0 {
                    break;
                }
                let reply = if data {
                    if line != ".\r\n" {
                        continue;
                    }
                    data = false;
                    "250 Queued\r\n".to_owned()
                } else if line.starts_with("EHLO") {
                    "250-test\r\n250 AUTH PLAIN\r\n".to_owned()
                } else if line.starts_with("AUTH") {
                    "235 Authenticated\r\n".to_owned()
                } else if line.starts_with("RCPT") {
                    format!("{code} Recipient\r\n")
                } else if line.starts_with("DATA") {
                    data = true;
                    "354 Continue\r\n".to_owned()
                } else if line.starts_with("QUIT") {
                    stream.get_mut().write_all(b"221 Bye\r\n").await.unwrap();
                    break;
                } else {
                    "250 OK\r\n".to_owned()
                };
                stream.get_mut().write_all(reply.as_bytes()).await.unwrap();
            }
        });
        (
            Server {
                host: "127.0.0.1".into(),
                port,
                security: imap::Security::Plain,
                trusted_cert: None,
            },
            task,
        )
    }

    fn test_account(store: &mut Store, smtp: Server) -> Account {
        let account = store
            .add_account(&NewAccount {
                kind: AccountKind::Imap,
                email: "me@example.com",
                name: "Me",
                label: "Test",
                config: &to_json(&AccountConfig::Imap {
                    username: "me@example.com".into(),
                    imap: Server::new("127.0.0.1", 993),
                    smtp,
                }),
            })
            .unwrap();
        store
            .replace_folders(
                account.id,
                &[RemoteFolder {
                    remote_id: "Drafts".into(),
                    parent: None,
                    name: "Drafts".into(),
                    role: Some(FolderRole::Drafts),
                    selectable: true,
                }],
            )
            .unwrap();
        account
    }

    #[tokio::test]
    async fn permanent_send_failure_recovers_exact_draft_and_files_across_restart() {
        for saved_before in [false, true] {
            let (smtp, delivery) = smtp_delivery(550).await;
            let path = std::env::temp_dir().join(format!(
                "spark-failed-send-{}.sqlite",
                drafts::new_message_id("test")
            ));
            let mut store = Store::open(&path).unwrap();
            let account = test_account(&mut store, smtp);
            let files = vec![OutboxFile {
                name: "plan.pdf".into(),
                mime: "application/pdf".into(),
                data: b"exact queued attachment".to_vec(),
            }];
            let mut draft = Draft {
                account: account.id,
                to: "Recipient <recipient@example.com>".into(),
                cc: "cc@example.com".into(),
                bcc: "bcc@example.com".into(),
                subject: "Latest subject".into(),
                body: "Latest edits before Send".into(),
                signature: Some("<p>Personal signature</p>".into()),
                signature_id: Some(42),
                quote: Some("Forwarded original".into()),
                in_reply_to: Some("original@example.com".into()),
                references: Some("earlier@example.com".into()),
                attachments: vec![OutgoingAttachment::File {
                    path: "/original-was-removed/plan.pdf".into(),
                    name: "plan.pdf".into(),
                    size: files[0].data.len() as u64,
                }],
                ..Default::default()
            };
            if saved_before {
                let old = Draft {
                    body: "Old autosave".into(),
                    ..draft.clone()
                };
                draft.draft_id = Some(drafts::save(&mut store, &account, &old, &[], 1).unwrap());
            }
            let id = store
                .queue_send(account.id, &to_json(&draft), 0, &files)
                .unwrap();
            if let Some(saved) = draft.draft_id {
                store.hold_draft_for_send(id, saved).unwrap();
            }
            let engine = Engine::new(Arc::new(Mutex::new(store)), Arc::new(TestSecrets));
            let outcomes = engine.send_due(account.id).await.unwrap();
            assert_eq!(outcomes.len(), 1);
            assert!(matches!(outcomes[0].result, Err(Error::Server(_))));
            let recovered = outcomes[0]
                .draft_id
                .expect("failure names its recovered draft");
            if let Some(saved) = draft.draft_id {
                assert_eq!(recovered, saved, "replace the stale autosave");
            }
            delivery.await.unwrap();
            drop(engine);

            let mut reopened = Store::open(&path).unwrap();
            draft.draft_id = Some(recovered);
            assert_eq!(drafts::open(&reopened, recovered).unwrap(), Some(draft));
            assert_eq!(reopened.messages().unwrap().len(), 1);
            assert_eq!(reopened.draft_files(recovered).unwrap(), files);
            assert!(reopened.outbox_files(id).unwrap().is_empty());
            assert!(
                reopened
                    .claim_due_sends(account.id, i64::MAX)
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                reopened.draft_uploads(account.id, i64::MAX).unwrap().len(),
                1
            );
            drop(reopened);
            std::fs::remove_file(path).unwrap();
        }
    }

    #[tokio::test]
    async fn successful_and_retryable_sends_keep_their_existing_lifecycle() {
        for code in [250, 450] {
            let (smtp, delivery) = smtp_delivery(code).await;
            let mut store = Store::open_in_memory().unwrap();
            let account = test_account(&mut store, smtp);
            let mut draft = Draft {
                account: account.id,
                to: "recipient@example.com".into(),
                body: "Send me".into(),
                ..Default::default()
            };
            let saved = drafts::save(&mut store, &account, &draft, &[], 1).unwrap();
            draft.draft_id = Some(saved);
            let id = store
                .queue_send(account.id, &to_json(&draft), 0, &[])
                .unwrap();
            store.hold_draft_for_send(id, saved).unwrap();
            let engine = Engine::new(Arc::new(Mutex::new(store)), Arc::new(TestSecrets));
            let outcomes = engine.send_due(account.id).await.unwrap();
            assert_eq!(outcomes.len(), 1);
            assert!(matches!(outcomes[0].result, Ok(sent) if sent == (code == 250)));
            assert!(outcomes[0].draft_id.is_none());
            delivery.await.unwrap();
            let mut store = engine.store();
            assert_eq!(store.is_draft(saved).unwrap(), code == 450);
            assert!(
                store.messages().unwrap().is_empty(),
                "retrying draft stays held"
            );
            if code == 450 {
                assert!(
                    store
                        .claim_due_sends(account.id, now_ms())
                        .unwrap()
                        .is_empty()
                );
                assert!(store.cancel_send(id).unwrap(), "retry remains cancellable");
                assert_eq!(drafts::open(&store, saved).unwrap(), Some(draft));
                assert_eq!(store.messages().unwrap().len(), 1, "Undo restores Drafts");
            }
        }
    }

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
    fn unscheduling_keeps_exactly_what_was_queued_in_drafts() {
        let mut store = Store::open_in_memory().unwrap();
        let account = test_account(&mut store, Server::new("127.0.0.1", 465));
        let queued = Draft {
            account: account.id,
            to: "ana@x.com".into(),
            subject: "Lease".into(),
            body: "As sent".into(),
            send_at: 9_000,
            send_later: Some(draft::SendLater {
                key: "tomorrow".into(),
                at: 9_000,
            }),
            remind: Some(draft::Remind {
                at: 20_000,
                if_no_reply: true,
            }),
            ..Default::default()
        };
        let file = OutboxFile {
            name: "plan.pdf".into(),
            mime: "application/pdf".into(),
            data: b"%PDF".to_vec(),
        };
        let json = serde_json::to_string(&queued).unwrap();
        let outbox = store
            .queue_send(account.id, &json, 9_000, std::slice::from_ref(&file))
            .unwrap();
        let engine = Engine::new(Arc::new(Mutex::new(store)), Arc::new(TestSecrets));

        let id = engine.unschedule(outbox, false).unwrap().unwrap();
        let store = engine.store();
        assert!(store.scheduled_sends().unwrap().is_empty());
        assert!(store.is_draft(id).unwrap());
        let saved: Draft = serde_json::from_str(&store.draft(id).unwrap().unwrap()).unwrap();
        assert_eq!((saved.body.as_str(), saved.send_later), ("As sent", None));
        assert_eq!(saved.remind, queued.remind);
        assert_eq!(store.draft_files(id).unwrap(), [file]);
        drop(store);
        // Gone from the queue: there's nothing left to take out.
        assert_eq!(engine.unschedule(outbox, true).unwrap(), None);
    }

    #[test]
    fn emptying_a_folder_pushes_what_moves_into_or_out_of_it() {
        let folder = |id, role| Folder {
            id,
            account_id: 1,
            remote_id: "x".into(),
            parent_id: None,
            name: "x".into(),
            role,
            selectable: true,
            sync_state: "{}".into(),
            synced_at: None,
        };
        let change = |place, home: Option<i64>, remote_place, remote_folder| Pending {
            id: 1,
            remote_id: None,
            mailbox: None,
            uid: None,
            message_id: None,
            place,
            folder: home.map(|id| (id, "x".to_owned())),
            unread: false,
            pinned: false,
            remote_place,
            remote_folder,
            remote_unread: false,
            remote_pinned: false,
        };
        let trash = folder(5, Some(FolderRole::Trash));
        let junk = folder(6, Some(FolderRole::Spam));
        assert!(moves_through(
            &change(Place::Trash, None, Place::Inbox, None),
            &trash
        ));
        assert!(moves_through(
            &change(Place::Inbox, None, Place::Trash, None),
            &trash
        ));
        assert!(!moves_through(
            &change(Place::Archive, None, Place::Inbox, None),
            &trash
        ));
        assert!(moves_through(
            &change(Place::Folder, Some(6), Place::Inbox, None),
            &junk
        ));
        assert!(moves_through(
            &change(Place::Inbox, None, Place::Folder, Some(6)),
            &junk
        ));
        assert!(!moves_through(
            &change(Place::Folder, Some(7), Place::Folder, Some(8)),
            &junk
        ));
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

    /// The stand-in's mailboxes: each one's UIDVALIDITY, and its messages
    /// by UID.
    type Held = HashMap<String, (u32, Vec<(u32, Vec<u8>)>)>;

    /// A local IMAP server for any number of sessions, counting sign-ins
    /// and messages sent. Tests change its mailboxes as they go.
    struct ImapStandIn {
        server: Server,
        held: Arc<Mutex<Held>>,
        logins: Arc<AtomicUsize>,
        fetches: Arc<AtomicUsize>,
        task: tokio::task::JoinHandle<()>,
    }

    impl ImapStandIn {
        async fn start(inbox_validity: u32, inbox: Vec<(u32, Vec<u8>)>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let server = Server {
                host: "127.0.0.1".into(),
                port: listener.local_addr().unwrap().port(),
                security: imap::Security::Plain,
                trusted_cert: None,
            };
            let held = Arc::new(Mutex::new(Held::from([(
                imap::INBOX.to_owned(),
                (inbox_validity, inbox),
            )])));
            let logins = Arc::new(AtomicUsize::new(0));
            let fetches = Arc::new(AtomicUsize::new(0));
            let shared = (held.clone(), logins.clone(), fetches.clone());
            let task = tokio::spawn(async move {
                while let Ok((socket, _)) = listener.accept().await {
                    let (held, logins, fetches) = shared.clone();
                    tokio::spawn(async move {
                        let _ = imap_session(socket, &held, &logins, &fetches).await;
                    });
                }
            });
            Self {
                server,
                held,
                logins,
                fetches,
                task,
            }
        }

        fn hold(&self, mailbox: &str, validity: u32, mail: Vec<(u32, Vec<u8>)>) {
            self.held
                .lock()
                .unwrap()
                .insert(mailbox.into(), (validity, mail));
        }

        /// Sign-ins and messages sent so far.
        fn counts(&self) -> (usize, usize) {
            (self.logins.load(SeqCst), self.fetches.load(SeqCst))
        }
    }

    impl Drop for ImapStandIn {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn imap_session(
        socket: TcpStream,
        held: &Mutex<Held>,
        logins: &AtomicUsize,
        fetches: &AtomicUsize,
    ) -> std::io::Result<()> {
        let (read, mut write) = socket.into_split();
        write.write_all(b"* OK stand-in\r\n").await?;
        let mut lines = BufReader::new(read).lines();
        let mut selected = String::new();
        while let Some(line) = lines.next_line().await? {
            let (tag, command) = line.split_once(' ').unwrap_or((&line, ""));
            let upper = command.to_ascii_uppercase();
            let mut reply = Vec::new();
            let mut status = "OK done";
            if upper.starts_with("LOGIN ") {
                logins.fetch_add(1, SeqCst);
            } else if upper.starts_with("SELECT ") {
                selected = command[7..].trim_matches('"').to_owned();
                match held.lock().unwrap().get(&selected) {
                    Some((validity, mail)) => reply.extend(
                        format!(
                            "* {} EXISTS\r\n* OK [UIDVALIDITY {validity}] ok\r\n",
                            mail.len()
                        )
                        .into_bytes(),
                    ),
                    None => status = "NO no such mailbox",
                }
            } else if upper.starts_with("UID SEARCH ") {
                let wanted = command.split('"').nth(1).unwrap_or_default();
                let hits: Vec<String> = held.lock().unwrap()[&selected]
                    .1
                    .iter()
                    .filter(|(_, raw)| String::from_utf8_lossy(raw).contains(wanted))
                    .map(|(uid, _)| uid.to_string())
                    .collect();
                reply.extend(format!("* SEARCH {}\r\n", hits.join(" ")).into_bytes());
            } else if upper.starts_with("UID FETCH ") {
                let uid: u32 = command.split_whitespace().nth(2).unwrap().parse().unwrap();
                let found = held.lock().unwrap()[&selected]
                    .1
                    .iter()
                    .enumerate()
                    .find(|(_, (u, _))| *u == uid)
                    .map(|(i, (_, raw))| (i + 1, raw.clone()));
                if let Some((seq, raw)) = found {
                    fetches.fetch_add(1, SeqCst);
                    let head = format!("* {seq} FETCH (UID {uid} BODY[] {{{}}}\r\n", raw.len());
                    write.write_all(head.as_bytes()).await?;
                    write.write_all(&raw).await?;
                    reply.extend(b")\r\n");
                }
            } else if upper == "LOGOUT" {
                write
                    .write_all(format!("* BYE\r\n{tag} OK bye\r\n").as_bytes())
                    .await?;
                return Ok(());
            }
            reply.extend(format!("{tag} {status}\r\n").into_bytes());
            write.write_all(&reply).await?;
        }
        Ok(())
    }

    const HTML: &str = "<p><img src=\"cid:logo\"><img src=\"cid:banner\"></p>";

    /// File `i` of `big_message`: `size` bytes of `fill`, but the first is `i`.
    fn part(fill: u8, i: usize, size: usize) -> Vec<u8> {
        let mut bytes = vec![fill; size];
        bytes[0] = i as u8;
        bytes
    }

    /// A message embedding two images and with four attached files, as
    /// `part` makes them.
    fn big_message(message_id: &str, fill: u8, size: usize) -> Vec<u8> {
        let base64 = |bytes: &[u8]| {
            let text = STANDARD.encode(bytes);
            let lines: Vec<&str> = text
                .as_bytes()
                .chunks(76)
                .map(|line| std::str::from_utf8(line).unwrap())
                .collect();
            lines.join("\r\n")
        };
        let mut raw = format!(
            "From: ana@example.com\r\nSubject: Files\r\nMessage-ID: <{message_id}>\r\n\
             Content-Type: multipart/mixed; boundary=m\r\n\r\n\
             --m\r\nContent-Type: multipart/related; boundary=r\r\n\r\n\
             --r\r\nContent-Type: text/html\r\n\r\n{HTML}\r\n"
        );
        for cid in ["logo", "banner"] {
            raw += &format!(
                "--r\r\nContent-Type: image/png\r\nContent-Disposition: inline\r\n\
                 Content-ID: <{cid}>\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n",
                base64(&[0x89, b'P', b'N', b'G', fill])
            );
        }
        raw += "--r--\r\n";
        for i in 0..4 {
            raw += &format!(
                "--m\r\nContent-Type: application/pdf; name=\"part{i}.pdf\"\r\n\
                 Content-Disposition: attachment; filename=\"part{i}.pdf\"\r\n\
                 Content-Transfer-Encoding: base64\r\n\r\n{}\r\n",
                base64(&part(fill, i, size))
            );
        }
        raw += "--m--\r\n";
        raw.into_bytes()
    }

    fn imap_account(store: &Store, server: &Server, validity: u32) -> Account {
        let account = store
            .add_account(&NewAccount {
                kind: AccountKind::Imap,
                email: "me@example.com",
                name: "Me",
                label: "Test",
                config: &to_json(&AccountConfig::Imap {
                    username: "me@example.com".into(),
                    imap: server.clone(),
                    smtp: server.clone(),
                }),
            })
            .unwrap();
        store
            .set_sync_state(account.id, &format!(r#"{{"uidValidity":{validity}}}"#))
            .unwrap();
        account
    }

    /// Stores `raw` as UID `uid` of `mailbox`, as a sync would.
    fn store_imap(store: &mut Store, account_id: i64, mailbox: &str, uid: u32, raw: &[u8]) -> i64 {
        let parsed = mail_parser::MessageParser::default().parse(raw).unwrap();
        let incoming = Incoming {
            account_id,
            mailbox: Some(mailbox.into()),
            uid: Some(uid),
            message_id: parsed.message_id().map(str::to_owned),
            from_address: "ana@example.com".into(),
            subject: "Files".into(),
            date: 1,
            html: Some(HTML.into()),
            attachments: attachment::describe(&parsed),
            ..Default::default()
        };
        match store.add_incoming(&incoming, false).unwrap() {
            Added::New(id) | Added::Known(id) => id,
        }
    }

    /// A folder of its own for a test's attachment cache.
    fn files_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("spark-engine-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// An engine keeping attachments in `dir`.
    fn caching(store: Arc<Mutex<Store>>, dir: &Path) -> Engine {
        Engine::new(store, Arc::new(TestSecrets)).with_attachment_cache(dir.to_owned())
    }

    /// The files kept in a cache folder.
    fn kept_files(dir: &Path) -> usize {
        std::fs::read_dir(dir).map_or(0, |found| {
            found
                .flatten()
                .map(|f| {
                    if f.path().is_dir() {
                        kept_files(&f.path())
                    } else {
                        1
                    }
                })
                .sum()
        })
    }

    #[tokio::test]
    async fn save_all_downloads_an_imap_email_once_and_keeps_its_images_too() {
        const SIZE: usize = 512 * 1024;
        let raw = big_message("big@example.com", 1, SIZE);
        assert!(raw.len() > 4 * SIZE, "a big message");
        let imap = ImapStandIn::start(7, vec![(42, raw.clone())]).await;
        let mut store = Store::open_in_memory().unwrap();
        let account = imap_account(&store, &imap.server, 7);
        let id = store_imap(&mut store, account.id, "INBOX", 42, &raw);
        let store = Arc::new(Mutex::new(store));

        // Without a cache: still one session and one download for all four.
        let bare = Engine::new(store.clone(), Arc::new(TestSecrets));
        let all = bare.attachments(id, &[0, 1, 2, 3]).await.unwrap();
        let names: Vec<&str> = all.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["part0.pdf", "part1.pdf", "part2.pdf", "part3.pdf"]);
        for (i, file) in all.iter().enumerate() {
            assert_eq!(file.bytes, part(1, i, SIZE));
            assert_eq!(file.mime, "application/pdf");
        }
        assert_eq!(imap.counts(), (1, 1), "one session, one download");
        // A forward of two, one of them twice.
        let forwarded = bare.attachments(id, &[1, 0, 1]).await.unwrap();
        assert_eq!(forwarded, [all[1].clone(), all[0].clone(), all[1].clone()]);
        assert_eq!(imap.counts(), (2, 2));
        assert!(bare.attachments(id, &[0, 4]).await.is_err(), "no fifth");

        let dir = files_dir("save-all");
        let engine = caching(store.clone(), &dir);
        assert_eq!(engine.attachments(id, &[0, 1, 2, 3]).await.unwrap(), all);
        assert_eq!(imap.counts(), (3, 3));
        assert_eq!(kept_files(&dir), 5, "four files and the images");

        // Opening or saving them again, or showing the images, uses what's kept.
        assert_eq!(engine.attachment(id, 2).await.unwrap(), all[2]);
        let again = engine.attachments(id, &[3, 0]).await.unwrap();
        assert_eq!(again, [all[3].clone(), all[0].clone()]);
        let images = engine.inline_images(id).await.unwrap();
        let cids: Vec<&str> = images.iter().map(|i| i.cid.as_str()).collect();
        assert_eq!(cids, ["logo", "banner"]);
        assert_eq!(images[0].bytes, [0x89, b'P', b'N', b'G', 1]);
        assert_eq!(engine.inline_images(id).await.unwrap(), images);
        assert_eq!(imap.counts(), (3, 3));

        // And after a restart.
        let restarted = caching(store.clone(), &dir);
        assert_eq!(restarted.attachment(id, 1).await.unwrap(), all[1]);
        assert_eq!(restarted.inline_images(id).await.unwrap(), images);
        assert_eq!(restarted.prefetch_attachments(id).await.unwrap(), 0);
        assert_eq!(imap.counts(), (3, 3));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn opening_an_email_downloads_it_once_for_its_files_and_images() {
        const SIZE: usize = 64 * 1024;
        let (one, two) = (
            big_message("one@example.com", 1, SIZE),
            big_message("two@example.com", 2, SIZE),
        );
        let imap = ImapStandIn::start(7, vec![(42, one.clone()), (43, two.clone())]).await;
        let mut store = Store::open_in_memory().unwrap();
        let account = imap_account(&store, &imap.server, 7);
        let a = store_imap(&mut store, account.id, "INBOX", 42, &one);
        let b = store_imap(&mut store, account.id, "INBOX", 43, &two);
        let dir = files_dir("open");
        let engine = caching(Arc::new(Mutex::new(store)), &dir);

        // Auto-download and the images, asked for as it opens.
        let (fetched, images) =
            tokio::join!(engine.prefetch_attachments(a), engine.inline_images(a));
        assert_eq!(fetched.unwrap(), 4);
        assert_eq!(images.unwrap().len(), 2);
        assert_eq!(imap.counts(), (1, 1));
        let (x, y, z) = tokio::join!(
            engine.attachment(a, 0),
            engine.attachment(a, 0),
            engine.inline_images(a)
        );
        assert_eq!(x.unwrap(), y.unwrap());
        assert_eq!(z.unwrap().len(), 2);
        assert_eq!(imap.counts(), (1, 1));
        // What's kept doesn't wait for a download under way.
        let email = engine.email_files(a).unwrap();
        let _downloading = engine.turn_at(&email, FILES).await;
        let wait = std::time::Duration::from_secs(5);
        let kept = tokio::time::timeout(wait, engine.attachment(a, 1)).await;
        assert_eq!(kept.unwrap().unwrap().bytes, part(1, 1, SIZE));
        assert!(
            tokio::time::timeout(wait, engine.inline_images(a))
                .await
                .is_ok()
        );
        drop(_downloading);

        // Shown first, with Auto-download off: its files are kept from that.
        assert_eq!(engine.inline_images(b).await.unwrap()[1].bytes[4], 2);
        assert_eq!(engine.prefetch_attachments(b).await.unwrap(), 0);
        assert_eq!(
            engine.attachment(b, 3).await.unwrap().bytes,
            part(2, 3, SIZE)
        );
        assert_eq!(imap.counts(), (2, 2));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_renumbered_mailbox_never_gives_another_emails_files() {
        const SIZE: usize = 1024;
        let first = big_message("first@example.com", 1, SIZE);
        let second = big_message("second@example.com", 2, SIZE);
        let imap = ImapStandIn::start(7, vec![(42, first.clone())]).await;
        let mut store = Store::open_in_memory().unwrap();
        let account = imap_account(&store, &imap.server, 7);
        let a = store_imap(&mut store, account.id, "INBOX", 42, &first);
        let projects = {
            store
                .replace_folders(
                    account.id,
                    &[RemoteFolder {
                        remote_id: "Projects".into(),
                        parent: None,
                        name: "Projects".into(),
                        role: None,
                        selectable: true,
                    }],
                )
                .unwrap();
            store.folders().unwrap()[0].id
        };
        store
            .set_folder_synced(projects, r#"{"uidValidity":3}"#, 1)
            .unwrap();
        imap.hold("Projects", 3, vec![(5, second.clone())]);
        let b = store_imap(&mut store, account.id, "Projects", 5, &second);
        let dir = files_dir("renumbered");
        let engine = caching(Arc::new(Mutex::new(store)), &dir);

        // The server renumbers both before a sync notices: the UIDs stored
        // now name the other email, which is found by Message-ID instead.
        imap.hold("INBOX", 8, vec![(42, second.clone()), (44, first.clone())]);
        imap.hold("Projects", 4, vec![(5, first.clone()), (6, second.clone())]);
        assert_eq!(
            engine.attachment(a, 0).await.unwrap().bytes,
            part(1, 0, SIZE)
        );
        assert_eq!(engine.inline_images(a).await.unwrap()[0].bytes[4], 1);
        assert_eq!(
            engine.attachment(b, 0).await.unwrap().bytes,
            part(2, 0, SIZE)
        );
        let eml = String::from_utf8(engine.raw_message(a).await.unwrap()).unwrap();
        assert!(eml.contains("<first@example.com>"));
        // Found that way, they aren't kept: asked for again, they come again.
        let (_, fetched) = imap.counts();
        assert_eq!(
            engine.attachment(a, 0).await.unwrap().bytes,
            part(1, 0, SIZE)
        );
        assert_eq!(imap.counts().1, fetched + 1);
        assert_eq!(kept_files(&dir), 0);

        // Once the sync has stored the new numbering, they're kept again.
        engine
            .store()
            .set_sync_state(account.id, r#"{"uidValidity":8}"#)
            .unwrap();
        store_imap(&mut engine.store(), account.id, "INBOX", 44, &first);
        for _ in 0..2 {
            assert_eq!(
                engine.attachment(a, 1).await.unwrap().bytes,
                part(1, 1, SIZE)
            );
        }
        assert_eq!(imap.counts().1, fetched + 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn graph_m1(path: &str) -> Option<serde_json::Value> {
        use serde_json::json;
        Some(match path.strip_prefix("/me/messages/m1/attachments")? {
            p if p.starts_with("?$select=id,name") => json!({ "value": [
                { "id": "a3", "name": "old.pdf", "contentType": "application/pdf", "size": 5 },
            ] }),
            p if p.starts_with('?') => json!({ "value": [
                { "id": "logo", "contentType": "image/png", "size": 10, "isInline": true },
            ] }),
            "/a1/$value" => json!("first"),
            "/a2/$value" => json!("second"),
            "/a3/$value" => json!("third"),
            "/a4/$value" => json!("fourth"),
            "/logo" => json!({ "contentType": "image/png", "contentId": "<logo>",
                               "contentBytes": "iVBORw0KGgo=" }),
            _ => return None,
        })
    }

    /// A Microsoft account signed in to the Graph stand-in at `base`, with
    /// message m1: four files, the last stored without its attachment id.
    fn graph_engine(base: String, dir: &Path) -> (Engine, i64) {
        let mut store = Store::open_in_memory().unwrap();
        let account = store
            .add_account(&NewAccount {
                kind: AccountKind::Microsoft,
                email: "me@contoso.com",
                name: "Me",
                label: "Work",
                config: &to_json(&AccountConfig::Microsoft {
                    client_id: "c".into(),
                    tenant: "t".into(),
                    calendar: false,
                }),
            })
            .unwrap();
        let attached = |name: &str, id: Option<&str>| Attachment {
            name: name.into(),
            mime: "application/pdf".into(),
            size: 5,
            remote_id: id.map(str::to_owned),
        };
        let incoming = Incoming {
            account_id: account.id,
            remote_id: Some("m1".into()),
            message_id: Some("m1@contoso.com".into()),
            from_address: "ana@contoso.com".into(),
            date: 1,
            html: Some("<img src=\"cid:logo\">".into()),
            attachments: vec![
                attached("one.pdf", Some("a1")),
                attached("two.pdf", Some("a2")),
                attached("four.pdf", Some("a4")),
                // Stored before attachment ids were: found by name.
                attached("old.pdf", None),
            ],
            ..Default::default()
        };
        let (Added::New(id) | Added::Known(id)) = store.add_incoming(&incoming, false).unwrap();
        let mut engine = caching(Arc::new(Mutex::new(store)), dir);
        engine.graph_base = base;
        engine.cache_token(
            account.id,
            &oauth::Tokens {
                access_token: "token".into(),
                refresh_token: None,
                expires_at: now_ms() + 3_600_000,
            },
        );
        (engine, id)
    }

    #[tokio::test]
    async fn graph_attachments_and_images_are_downloaded_once() {
        let (base, asked) = graph::tests::stand_in(graph_m1).await;
        let dir = files_dir("graph");
        let (engine, id) = graph_engine(base, &dir);
        let asked_for = || std::mem::take(&mut *asked.lock().unwrap());

        let files = engine.attachments(id, &[0, 1, 2, 3]).await.unwrap();
        let bytes: Vec<&[u8]> = files.iter().map(|f| f.bytes.as_slice()).collect();
        assert_eq!(
            bytes,
            [
                &b"\"first\""[..],
                b"\"second\"",
                b"\"fourth\"",
                b"\"third\""
            ]
        );
        assert_eq!(asked_for().len(), 5, "old.pdf's listing, then the four");
        assert_eq!(engine.attachments(id, &[0, 1, 2, 3]).await.unwrap(), files);
        assert_eq!(engine.attachment(id, 1).await.unwrap(), files[1]);
        assert_eq!(engine.prefetch_attachments(id).await.unwrap(), 0);
        assert!(asked_for().is_empty());

        let images = engine.inline_images(id).await.unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(asked_for().len(), 2, "the list, then the image");
        assert_eq!(engine.inline_images(id).await.unwrap(), images);
        assert!(asked_for().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_drafts_images_come_from_the_server_each_time() {
        // The draft's logo on the server: edited elsewhere, it changes
        // under the same message and Content-ID.
        static LOGO: AtomicU8 = AtomicU8::new(1);
        fn draft_d1(path: &str) -> Option<serde_json::Value> {
            use serde_json::json;
            Some(match path.strip_prefix("/me/messages/d1/attachments")? {
                p if p.starts_with('?') => json!({ "value": [
                    { "id": "logo", "contentType": "image/png", "size": 10, "isInline": true },
                ] }),
                "/logo" => json!({ "contentType": "image/png", "contentId": "<logo>",
                    "contentBytes": STANDARD.encode([0x89, b'P', b'N', b'G', LOGO.load(SeqCst)]) }),
                _ => return None,
            })
        }
        let (base, _) = graph::tests::stand_in(draft_d1).await;
        let dir = files_dir("graph-draft");
        let (engine, m1) = graph_engine(base, &dir);
        let draft = {
            let mut store = engine.store();
            let account = store.message(m1).unwrap().unwrap().account_id;
            store
                .replace_folders(
                    account,
                    &[RemoteFolder {
                        remote_id: "drafts".into(),
                        parent: None,
                        name: "Drafts".into(),
                        role: Some(FolderRole::Drafts),
                        selectable: true,
                    }],
                )
                .unwrap();
            let folder = store.folders().unwrap()[0].id;
            let incoming = Incoming {
                account_id: account,
                remote_id: Some("d1".into()),
                message_id: Some("d1@contoso.com".into()),
                from_address: "me@contoso.com".into(),
                date: 1,
                html: Some("<img src=\"cid:logo\">".into()),
                ..Default::default()
            };
            let (Added::New(id) | Added::Known(id)) =
                store.add_folder_message(folder, &incoming).unwrap();
            assert!(store.is_draft(id).unwrap());
            id
        };
        let logo = |images: Vec<InlineImage>| images[0].bytes[4];

        assert_eq!(logo(engine.inline_images(draft).await.unwrap()), 1);
        LOGO.store(2, SeqCst);
        assert_eq!(logo(engine.inline_images(draft).await.unwrap()), 2);
        // Nor are they kept for after a restart.
        assert_eq!(kept_files(&dir), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
