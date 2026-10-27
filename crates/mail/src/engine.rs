//! The engine the apps drive: adding accounts, and per account one round of
//! work at a time (send what's due, push local changes, fetch new mail,
//! reach back for older mail, list and sync folders). Callers run rounds
//! for an account one after another; different accounts can run in
//! parallel. Between rounds, while nothing else waits, the background
//! download brings in a year of older inbox mail a step at a time, without
//! bodies; a body downloads when its email is opened.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use async_imap::types::Fetch;
use futures_util::TryStreamExt;
use lettre::message::Mailbox;
use serde::{Deserialize, Serialize};
use spark_store::{
    Account, AccountKind, Folder, FolderRole, Location, NewAccount, NewReminder, Pending, Place,
    Query, Store, now_ms,
};
use tokio::sync::{Mutex as AsyncMutex, Notify};

use crate::attachment::{self, AttachmentData, InlineImage};
use crate::cache::{AttachmentCache, Flight};
use crate::calendar::{self, Event, EventDraft, Response, Scope, Zone};
use crate::draft::{self, Draft, Original, OutgoingAttachment};
use crate::drafts::{self, DraftsReport};
use crate::graph::{self, Graph};
use crate::imap::{self, Server};
use crate::invite::{self, Answer, Invite};
use crate::labels;
use crate::oauth::{self, App};
use crate::recurrence::{self, Repeat};
use crate::unsubscribe::{self, Unsubscribed};
use crate::{Error, OLDER_STEP, Result, Service, parse, search, smtp};

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
    /// With `fetch`, also fetch what Microsoft mail was stored without
    /// (`Graph::repair`).
    pub repair: bool,
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

/// How `Engine::watch_inbox` ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Watch {
    /// It can't watch this account: Microsoft, or a server without IDLE.
    Unwatched,
    /// It was told to start over, e.g. after the computer slept.
    Restart,
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
    /// Mail stored without its attachments or invite, as Microsoft couldn't
    /// give them then, that has them now (`graph::Graph::repair`).
    pub repaired: usize,
    /// New inbox mail an IMAP server wouldn't give this round, even
    /// message by message: the next round that fetches asks for it again.
    pub skipped: usize,
    /// Older Microsoft inbox mail that a pass over it found changed
    /// elsewhere: read, flagged, moved or deleted
    /// (`graph::Graph::reconcile_inbox`).
    pub reconciled: usize,
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
            + self.repaired
            + self.reconciled
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

/// What `Engine::delete_own_draft` did with an auto-draft.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnDraft {
    /// Deleted here and, as this app put it there, on the server.
    Deleted,
    /// Its copy on the server isn't the one this app put there: another app
    /// changed or replaced it, so it's the user's.
    Changed,
    /// Not now: `may` said no, or this server can't delete only that copy.
    Kept,
}

/// The outcome of one queued draft.
#[derive(Debug)]
pub struct SendOutcome {
    pub id: i64,
    /// The editable draft recovered after a permanent failure.
    pub draft_id: Option<i64>,
    /// The email it replies to, if it's a reply.
    pub reply_to: Option<i64>,
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

/// How far back the background download of older inbox mail reaches: a
/// year, like Spark's. Mail older than that comes in as the user scrolls
/// to it.
pub const BACKFILL_DAYS: i64 = 366;
/// The background download's pause between steps, so new mail and what
/// the user does go first.
const BACKFILL_PAUSE: Duration = Duration::from_secs(2);
/// A failed step is tried again after this long, twice as long after each
/// failure in a row, up to `BACKFILL_LONGEST_WAIT`.
const BACKFILL_RETRY: Duration = Duration::from_secs(30);
const BACKFILL_LONGEST_WAIT: Duration = Duration::from_secs(15 * 60);
/// The longest a background download step may take: it holds the account's
/// turn. Its requests give up sooner; this is for the rest of it.
const BACKFILL_STEP_TIME: Duration = Duration::from_secs(45);

/// How often a pass goes over the Microsoft inbox mail stored from before
/// the delta query's window (`graph::Graph::reconcile_inbox`), besides once
/// after the app starts.
const RECONCILE_EVERY: Duration = Duration::from_secs(60 * 60);

/// When an account's next background download step may start (`Engine::backfill`).
#[derive(Default)]
struct Pace {
    next: Option<Instant>,
    /// Steps that failed in a row.
    failures: u32,
    /// When the account's last pass over older inbox mail ended
    /// (`graph::Graph::reconcile_inbox`): none since the app started.
    reconciled: Option<Instant>,
    /// What the account's rounds have tried to push (`Engine::work_waits`).
    tried: Tried,
}

/// What an account's rounds have tried to push since the app started:
/// what's still there of it failed, and waits for a round to try it again,
/// not holding up the background download meanwhile.
#[derive(Clone, Debug, Default)]
struct Tried {
    /// Local changes made by then (Unix ms).
    changes: Option<i64>,
    /// Drafts saved by then.
    drafts: Option<i64>,
    /// Deletions of drafts' server copies (`DraftRemoval::id`).
    removals: HashSet<i64>,
}

impl Pace {
    /// Whether a new pass over older inbox mail is due.
    fn reconcile_due(&self) -> bool {
        self.reconciled
            .is_none_or(|ended| ended.elapsed() >= RECONCILE_EVERY)
    }
}

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
    /// What each Microsoft account's rounds and searches learn of Graph
    /// for the next ones (`graph::Standing`), in memory only.
    standings: Mutex<HashMap<i64, Arc<graph::Standing>>>,
    /// Downloaded attachments kept on this computer, if they're kept.
    files: Option<Arc<AttachmentCache>>,
    /// Where Graph is: `graph::GRAPH`, or a stand-in's address in tests.
    graph_base: String,
    /// Each account's background download pace (`backfill`), in memory only:
    /// a restart starts it again where the sync state says it got to.
    paces: Mutex<HashMap<i64, Pace>>,
    /// Per message whose body is downloading (`fetch_body`), taken by each
    /// asker in turn, so it's downloaded once.
    bodies: Mutex<HashMap<i64, Arc<AsyncMutex<()>>>>,
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
            standings: Mutex::new(HashMap::new()),
            files: None,
            graph_base: graph::GRAPH.to_owned(),
            paces: Mutex::new(HashMap::new()),
            bodies: Mutex::new(HashMap::new()),
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
        self.standings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&account_id);
        let _ = self.secrets.set(&password_key(account_id), "");
        let _ = self.secrets.set(&token_key(account_id), "");
        Ok(())
    }

    /// One round for an account: push changes older than the undo window,
    /// and with `fetch`, bring in new mail and what mail stored earlier came
    /// without. Opens no connection when there is nothing to do.
    pub async fn run(&self, account_id: i64, fetch: bool) -> Result<Report> {
        self.run_round(
            account_id,
            &Round {
                fetch,
                repair: fetch,
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
        let now = now_ms();
        let pending = self.store().pending(account_id, now, SETTLE_MS)?;
        // Saves settle like other changes, so a burst of them goes up once.
        let settled = now - SETTLE_MS;
        let draft_work = self.store().has_draft_work(account_id, settled)?;
        let label_work = self.store().has_label_work(account_id)?;
        let removals: HashSet<i64> = if draft_work {
            let removals = self.store().draft_removals(account_id)?;
            removals.iter().map(|r| r.id).collect()
        } else {
            HashSet::new()
        };
        let tried_drafts = |tried: &mut Tried| {
            tried.drafts = Some(settled);
            tried.removals = removals.clone();
        };
        // A round the server stops at its changes doesn't get to its drafts,
        // which wait for one that does as the changes do.
        let tried_changes = |tried: &mut Tried, failed: bool| {
            tried.changes = Some(settled);
            if failed {
                tried_drafts(tried);
            }
        };
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
            && !label_work
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
                let pushed = imap::push(&mut session, &self.store, &mut state, pending).await;
                self.tried(account_id, &pushed, |t| tried_changes(t, pushed.is_err()));
                report.pushed = pushed?;
                if draft_work {
                    let pushed =
                        drafts::push_imap(&mut session, &self.store, &account, &state, settled)
                            .await;
                    self.tried(account_id, &pushed, tried_drafts);
                    report.drafts_pushed(pushed?);
                } else {
                    self.tried(account_id, &Ok(()), tried_drafts);
                }
                if label_work {
                    labels::push_imap(&mut session, &self.store, &state, account_id).await?;
                }
                if round.fetch {
                    let sync =
                        imap::sync_inbox(&mut session, &self.store, account_id, &mut state).await;
                    // Keep the position even if part of the sync failed.
                    self.store().set_sync_state(account_id, &to_json(&state))?;
                    let sync = sync?;
                    report.headers += sync.headers;
                    report.skipped = sync.skipped;
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
                let graph = self.syncing_graph(account_id, &client_id, &tenant).await?;
                let pushed = graph.push(&self.store, pending).await;
                self.tried(account_id, &pushed, |t| tried_changes(t, pushed.is_err()));
                report.pushed = pushed?;
                if draft_work {
                    let pushed = drafts::push_graph(&graph, &self.store, &account, settled).await;
                    self.tried(account_id, &pushed, tried_drafts);
                    report.drafts_pushed(pushed?);
                } else {
                    self.tried(account_id, &Ok(()), tried_drafts);
                }
                if label_work {
                    labels::push_graph(&graph, &self.store, account_id).await?;
                }
                // Its repair list is the store's: syncs and searches add to
                // it as they go, and `save_state` leaves it be.
                let mut state: graph::SyncState = from_json(&account.sync_state);
                if round.fetch {
                    let sync = graph
                        .sync_inbox(&self.store, account_id, &mut state)
                        .await?;
                    graph::save_state(&self.store(), account_id, &state)?;
                    report.headers += sync.headers;
                    report.synced(sync.new, sync.gone, sync.first);
                }
                if round.older_inbox {
                    let older = graph.older_inbox(&self.store, account_id, &mut state).await;
                    // Keep how far it got even if part of it failed.
                    graph::save_state(&self.store(), account_id, &state)?;
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
                // Last, and only on rounds that ask for it (the app's
                // sweeps). Rounds that only push come with every change the
                // user makes, and inbox checks every half minute; up to a
                // minute of repairs would hold up what they're for.
                if round.fetch && round.repair {
                    report.repaired = graph.repair(&self.store, account_id, now_ms()).await;
                    // Mail searches stored by REST id before they translated
                    // what they found; a failure is tried again next sweep.
                    let _ = graph.rekey_rest_ids(&self.store, account_id).await;
                }
            }
        }
        Ok(report)
    }

    /// Whether the account's background download has a step to take now
    /// (`backfill`): what's left of it (`backfill_left`), the pause since
    /// its last step is over, Microsoft isn't asking the account to wait,
    /// and nothing else waits for it (`work_waits`).
    pub fn backfill_due(&self, account_id: i64) -> bool {
        let paused = self
            .paces_lock()
            .get(&account_id)
            .and_then(|pace| pace.next)
            .is_some_and(|next| Instant::now() < next);
        let held = self
            .standings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&account_id)
            .is_some_and(|standing| standing.is_paused());
        !paused && !held && self.backfill_left(account_id) && !self.work_waits(account_id)
    }

    /// Whether the account's background download has anything left to do:
    /// its first sync is done, and its inbox isn't covered back to
    /// `BACKFILL_DAYS` yet nor complete, or (Microsoft) a pass over the
    /// older mail it has is under way or due: every `RECONCILE_EVERY`, and
    /// once after the app starts.
    pub fn backfill_left(&self, account_id: i64) -> bool {
        let reconcile = self
            .paces_lock()
            .get(&account_id)
            .is_none_or(Pace::reconcile_due);
        let Ok(Some(account)) = self.store().account(account_id) else {
            return false;
        };
        match config(&account) {
            Ok(AccountConfig::Imap { .. }) => imap::backfill_due(&from_json(&account.sync_state)),
            Ok(AccountConfig::Microsoft { .. }) => {
                let state: graph::SyncState = from_json(&account.sync_state);
                graph::backfill_due(&state, backfill_since(now_ms()))
                    || (graph::reconcile_due(&state) && (reconcile || state.reconcile.is_some()))
            }
            Err(_) => false,
        }
    }

    /// Whether something the user did waits for the account, which goes
    /// before the background download: a local change or a saved draft to
    /// push, even one still in its undo window, or mail to send. What a
    /// round tried and couldn't push waits for the next round instead
    /// (`Tried`), so a change the server won't take (Done where it can't
    /// move mail) doesn't hold up the download for good.
    fn work_waits(&self, account_id: i64) -> bool {
        let tried = self
            .paces_lock()
            .get(&account_id)
            .map(|pace| pace.tried.clone())
            .unwrap_or_default();
        let store = self.store();
        !store
            .has_pending_after(account_id, tried.changes.unwrap_or(i64::MIN))
            .is_ok_and(|any| !any)
            || !store
                .has_draft_work_since(
                    account_id,
                    tried.drafts.unwrap_or(i64::MIN),
                    &tried.removals,
                )
                .is_ok_and(|work| !work)
            || !store
                .has_due_sends(account_id, now_ms())
                .is_ok_and(|due| !due)
    }

    /// Notes what a round tried to push, given how it went (`Tried`): not
    /// if it couldn't reach the server, as then it's to be tried first.
    fn tried<T>(&self, account_id: i64, pushed: &Result<T>, note: impl FnOnce(&mut Tried)) {
        if !matches!(pushed, Err(Error::Network { .. })) {
            note(&mut self.paces_lock().entry(account_id).or_default().tried);
        }
    }

    /// One step of the account's background download of older inbox mail:
    /// reaching back `OLDER_STEP` messages, without their bodies, as
    /// scrolling does, while what's stored doesn't go back `BACKFILL_DAYS`.
    /// Then, on a Microsoft account, a page of a pass over that older mail,
    /// which the delta query doesn't see (`graph::Graph::reconcile_inbox`).
    /// Where they got to is kept with the sync state, so a restart goes on
    /// from there. It takes the account's turn, like a round, and keeps to
    /// Microsoft's pause. The next step waits `BACKFILL_PAUSE`, or after a
    /// failure longer each time (`backfill_due`). The report says what it
    /// brought in, as `older`, and what the pass changed, as `reconciled`.
    ///
    /// What the user does goes first: with the turn, if something of theirs
    /// waits for the account (a round had the turn meanwhile), it does
    /// nothing. A step that takes over `BACKFILL_STEP_TIME` fails.
    pub async fn backfill(&self, account_id: i64) -> Result<Report> {
        let turn = self.turn(account_id);
        let _turn = turn.lock().await;
        if self.work_waits(account_id) {
            return Ok(Report::default());
        }
        let reconcile = self
            .paces_lock()
            .get(&account_id)
            .is_none_or(Pace::reconcile_due);
        let stepped = tokio::time::timeout(
            BACKFILL_STEP_TIME,
            self.backfill_step(account_id, reconcile),
        )
        .await
        .unwrap_or_else(|_| {
            Err(Error::Server(
                "Downloading older mail took too long.".into(),
            ))
        });
        let mut paces = self.paces_lock();
        let pace = paces.entry(account_id).or_default();
        let wait = match &stepped {
            Ok((_, ended)) => {
                pace.failures = 0;
                if *ended {
                    pace.reconciled = Some(Instant::now());
                }
                BACKFILL_PAUSE
            }
            Err(_) => {
                pace.failures = pace.failures.saturating_add(1);
                BACKFILL_RETRY
                    .saturating_mul(1 << pace.failures.min(6).saturating_sub(1))
                    .min(BACKFILL_LONGEST_WAIT)
            }
        };
        pace.next = Some(Instant::now() + wait);
        Ok(stepped?.0)
    }

    /// `backfill`'s step: what it did, and whether a pass over older mail
    /// ended. `reconcile`: a new pass may start.
    async fn backfill_step(&self, account_id: i64, reconcile: bool) -> Result<(Report, bool)> {
        let account = self.account(account_id)?;
        let since = backfill_since(now_ms());
        match config(&account)? {
            AccountConfig::Imap {
                username,
                imap: server,
                ..
            } => {
                let mut state: imap::SyncState = from_json(&account.sync_state);
                if !imap::backfill_due(&state) {
                    return Ok(Default::default());
                }
                let password = self.password(account_id)?;
                let mut session = imap::login(&server, &username, &password).await?;
                let older =
                    imap::backfill_inbox(&mut session, &self.store, account_id, &mut state, since)
                        .await;
                // Keep the lowered floor even if part of it failed.
                self.store().set_sync_state(account_id, &to_json(&state))?;
                let _ = session.logout().await;
                let report = Report {
                    older: older?,
                    ..Default::default()
                };
                Ok((report, false))
            }
            AccountConfig::Microsoft {
                client_id, tenant, ..
            } => {
                let mut state: graph::SyncState = from_json(&account.sync_state);
                let older = graph::backfill_due(&state, since);
                let reconcile =
                    graph::reconcile_due(&state) && (reconcile || state.reconcile.is_some());
                // Not waiting out Microsoft's pause with the account's turn.
                if (!older && !reconcile) || self.standing(account_id).is_paused() {
                    return Ok(Default::default());
                }
                let graph = self.syncing_graph(account_id, &client_id, &tenant).await?;
                if older {
                    let older = graph
                        .older_inbox_step(&self.store, account_id, &mut state)
                        .await;
                    // Keep how far it got even if part of it failed.
                    graph::save_state(&self.store(), account_id, &state)?;
                    let report = Report {
                        older: older?,
                        ..Default::default()
                    };
                    return Ok((report, false));
                }
                let reconciled = graph
                    .reconcile_inbox(&self.store, account_id, &mut state)
                    .await;
                graph::save_state(&self.store(), account_id, &state)?;
                let reconciled = reconciled?;
                let report = Report {
                    reconciled: reconciled.changed,
                    ..Default::default()
                };
                Ok((report, reconciled.done))
            }
        }
    }

    /// Downloads the body of a stored message that came without it
    /// (`Body::Missing`: reaching back, and the background
    /// download), and fills it in (`Store::fill_in_body`) with its
    /// attachments and invite. An IMAP email comes whole, once, and the
    /// files in it are kept too, so opening them doesn't download it again.
    /// It runs beside the account's rounds, on a connection of its own, and
    /// isn't held back by Microsoft's pause: the user is waiting for it.
    /// Several asking at once download it once. Returns whether it was
    /// filled in: false if it had its body already, or by then.
    pub async fn fetch_body(&self, message_id: i64) -> Result<bool> {
        if !self.body_missing(message_id)? {
            return Ok(false);
        }
        let flight = self.bodies_lock().entry(message_id).or_default().clone();
        let _flight = flight.lock().await;
        let fetched = match self.body_missing(message_id) {
            Ok(true) => self.download_body(message_id).await,
            other => other.map(|_| false),
        };
        // The last asker to have its turn takes it off the list: those
        // waiting meanwhile take theirs on the same one.
        let mut bodies = self.bodies_lock();
        if Arc::strong_count(&flight) <= 2 {
            bodies.remove(&message_id);
        }
        fetched
    }

    /// Whether a stored message waits for its body.
    fn body_missing(&self, message_id: i64) -> Result<bool> {
        let content = self.store().content(message_id)?;
        let content =
            content.ok_or_else(|| Error::Config("That email is no longer here.".into()))?;
        Ok(content.body_missing)
    }

    /// `fetch_body`'s download, once it's this one's turn.
    async fn download_body(&self, message_id: i64) -> Result<bool> {
        let (at, validity) = self.locate(message_id)?;
        let account = self.account(at.account_id)?;
        match config(&account)? {
            AccountConfig::Imap {
                username,
                imap: server,
                ..
            } => {
                let (raw, trusted) = self
                    .fetch_raw(&account, &server, &username, &at, validity)
                    .await?;
                let (raw, whole) = blocking(move || {
                    let whole = parse::parse_message(&raw);
                    (raw, whole)
                })
                .await?;
                let whole = whole.ok_or_else(|| {
                    Error::Server("The email on the server is unreadable.".into())
                })?;
                // What's filled in stays: not a changed copy sent again
                // with its Message-ID, found as its UID was gone.
                if !imap::same_email(&at, &whole) {
                    return Err(Error::Server(imap::NOT_THE_ONE.into()));
                }
                let filled = self.store().fill_in_body(message_id, &whole)?;
                // Not what was found by Message-ID in a renumbered mailbox
                // (`imap::fetch_raw_checked`), as `download_imap` doesn't.
                if filled && trusted {
                    self.keep_whole(message_id, raw).await;
                }
                Ok(filled)
            }
            AccountConfig::Microsoft {
                client_id, tenant, ..
            } => {
                let remote = at.remote_id.ok_or(Error::Gone)?;
                self.graph(account.id, &client_id, &tenant)
                    .await?
                    .fetch_body(&self.store, account.id, message_id, &remote)
                    .await
            }
        }
    }

    /// Keeps the attachments and embedded images of an IMAP email that was
    /// just downloaded whole (`fetch_body`), as `download_imap` keeps them.
    /// Nothing if there's no cache, or nothing to keep; a failure only
    /// leaves them to be downloaded when they're opened.
    async fn keep_whole(&self, message_id: i64, raw: Vec<u8>) {
        if self.files.is_none() {
            return;
        }
        let Ok(email) = self.email_files(message_id) else {
            return;
        };
        if email.attachments.is_empty() && email.images.is_empty() {
            return;
        }
        // Auto-download, which opening it starts, waits for these.
        let _turn = self.turn_at(&email, FILES).await;
        let names: Vec<String> = email.attachments.iter().map(|a| a.name.clone()).collect();
        let cids = email.images.clone();
        let extracted = blocking(move || {
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            attachment::extract_files(&raw, &names, &cids)
        })
        .await;
        if let Ok((files, images)) = extracted {
            let _ = self.keep(&email, files, Some(images)).await;
        }
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

    /// Watches an IMAP account's inbox with IDLE, on a connection of its
    /// own, calling `changed` whenever the server reports a change there,
    /// and once as it connects: mail that came while nothing watched is
    /// below what IDLE reports. Sets `watching` as the IDLE starts; the
    /// caller clears it once this returns. Returns `Unwatched` at once for
    /// Microsoft accounts and servers without IDLE, `Restart` once
    /// `restart` is notified (connecting too), and otherwise runs until the
    /// connection fails. It only reads, so it doesn't take the account's
    /// turn.
    pub async fn watch_inbox(
        &self,
        account_id: i64,
        restart: &Notify,
        watching: &AtomicBool,
        changed: impl Fn(),
    ) -> Result<Watch> {
        let account = self.account(account_id)?;
        let AccountConfig::Imap {
            username,
            imap: server,
            ..
        } = config(&account)?
        else {
            return Ok(Watch::Unwatched);
        };
        let password = self.password(account_id)?;
        let connect = async {
            let mut session = imap::login(&server, &username, &password).await?;
            if !imap::can_idle(&mut session).await? {
                let _ = session.logout().await;
                return Ok(None);
            }
            imap::examine_inbox(&mut session).await?;
            Ok::<_, Error>(Some(session))
        };
        let mut session = tokio::select! {
            session = connect => match session? {
                Some(session) => session,
                None => return Ok(Watch::Unwatched),
            },
            () = restart.notified() => return Ok(Watch::Restart),
        };
        changed();
        watching.store(true, Ordering::Relaxed);
        loop {
            session = match imap::idle(session, restart.notified(), imap::IDLE_RENEW).await? {
                imap::Idle::Changed(session) => {
                    changed();
                    session
                }
                imap::Idle::Quiet(session) => session,
                imap::Idle::Stopped => return Ok(Watch::Restart),
            };
        }
    }

    /// Puts a search (in the search field's syntax) to the account's server
    /// and stores the mail it finds that isn't stored yet, so the store's
    /// search has it (see the `search` module), and the bodies of what it
    /// finds stored without them. Returns how many messages were new or
    /// filled in. It runs beside the account's rounds, on a connection of
    /// its own; on a Microsoft account, they fetch what Graph couldn't give
    /// of the mail it stores yet, such as its attachments.
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
                self.syncing_graph(account_id, &client_id, &tenant)
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

    /// What the account's rounds and searches learned of Graph (`standings`).
    fn standing(&self, account_id: i64) -> Arc<graph::Standing> {
        self.standings
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

    /// A Graph client for the account's rounds and searches, keeping to
    /// what the ones before learned (`standings`). While Graph asks the
    /// account to wait, it waits for the end before signing in, or fails as
    /// busy at once if that's more than half a minute off. What the user
    /// does themselves, like sending or opening a file, isn't held back.
    async fn syncing_graph(&self, account_id: i64, client_id: &str, tenant: &str) -> Result<Graph> {
        let standing = self.standing(account_id);
        standing.paused().await?;
        Ok(self
            .graph(account_id, client_id, tenant)
            .await?
            .keeping(standing))
    }

    /// Deletes auto-draft `id`, here and on the server, if the server's copy
    /// is still exactly the one this app put there (`Store::auto_draft_uploaded`):
    /// on Microsoft, the same changeKey, as Outlook edits drafts in place;
    /// on IMAP, the only message with its Message-ID in its mailbox, at the
    /// UID it went to, as an edit there is a new message that may keep the
    /// Message-ID (Roundcube does). `may` decides last, with the store held.
    /// Only that copy goes: never one that replaced it.
    pub async fn delete_own_draft(
        &self,
        id: i64,
        may: impl FnOnce(&Store) -> bool,
    ) -> Result<OwnDraft> {
        let Some(account_id) = self.store().location(id)?.map(|at| at.account_id) else {
            return Err(Error::Gone);
        };
        // One change to the server at a time, as rounds make theirs. Where
        // its copy is, a round waited for may just have changed (uploaded
        // it), so that's read once it's this one's turn.
        let turn = self.turn(account_id);
        let _turn = turn.lock().await;
        let (at, ours) = {
            let store = self.store();
            let Some(at) = store.location(id)? else {
                return Err(Error::Gone);
            };
            (at, store.auto_draft_server_version(id)?)
        };
        let account = self.account(at.account_id)?;
        match config(&account)? {
            AccountConfig::Microsoft {
                client_id, tenant, ..
            } => {
                if let Some(remote) = &at.remote_id {
                    let Some(ours) = ours else {
                        return Ok(OwnDraft::Changed);
                    };
                    let graph = self.syncing_graph(account.id, &client_id, &tenant).await?;
                    if graph.change_key(remote).await?.as_deref() != Some(ours.as_str()) {
                        return Ok(OwnDraft::Changed);
                    }
                    {
                        let store = self.store();
                        if !may(&store) {
                            return Ok(OwnDraft::Kept);
                        }
                        // Gone from here now, with nothing queued: a later
                        // round couldn't tell an edit made in Outlook since.
                        store.remove_messages(&[id])?;
                    }
                    // Left on the server, it comes back as a draft of the user's.
                    if let Err(e) = graph.delete_draft(remote).await {
                        eprintln!(
                            "spark-rs: couldn't delete an auto-draft's copy on the server: {e}"
                        );
                    }
                    return Ok(OwnDraft::Deleted);
                }
                // Not on the server yet: only here.
                let mut store = self.store();
                if !may(&store) {
                    return Ok(OwnDraft::Kept);
                }
                store.discard_draft(id)?;
                Ok(OwnDraft::Deleted)
            }
            AccountConfig::Imap {
                username,
                imap: server,
                ..
            } => {
                let Some(mailbox) = at.mailbox.clone() else {
                    // Never uploaded: only here.
                    let mut store = self.store();
                    if !may(&store) {
                        return Ok(OwnDraft::Kept);
                    }
                    store.discard_draft(id)?;
                    return Ok(OwnDraft::Deleted);
                };
                let place = ours
                    .as_deref()
                    .and_then(|version| drafts::imap_place(version, &mailbox));
                let (Some(place), Some(message_id)) = (place, at.message_id.clone()) else {
                    // Uploaded, but where to wasn't learned: can't tell.
                    return Ok(OwnDraft::Changed);
                };
                let password = self.password(account.id)?;
                let mut session = imap::login(&server, &username, &password).await?;
                let deleted = self
                    .delete_own_imap_copy(&mut session, id, &mailbox, place, &message_id, may)
                    .await;
                let _ = session.logout().await;
                deleted
            }
        }
    }

    /// `delete_own_draft` on IMAP: draft `id`'s copy in `mailbox`, at the
    /// UIDVALIDITY and UID `place` it went up as.
    async fn delete_own_imap_copy(
        &self,
        session: &mut imap::Session,
        id: i64,
        mailbox: &str,
        (validity, uid): (u32, u32),
        message_id: &str,
        may: impl FnOnce(&Store) -> bool,
    ) -> Result<OwnDraft> {
        // Without UIDPLUS, an expunge could take other clients' deleted mail.
        if !session.capabilities().await?.has_str("UIDPLUS") {
            return Ok(OwnDraft::Kept);
        }
        match session.select(mailbox).await {
            // Renumbered since (the mailbox restored or made again): its UID
            // could be another message's now.
            Ok(selected) if selected.uid_validity != Some(validity) => {
                return Ok(OwnDraft::Changed);
            }
            Ok(_) => {}
            Err(async_imap::error::Error::No(_)) => return Ok(OwnDraft::Changed),
            Err(e) => return Err(e.into()),
        }
        if imap::find_message_id(session, message_id).await? != [uid] {
            return Ok(OwnDraft::Changed);
        }
        {
            let store = self.store();
            if !may(&store) {
                return Ok(OwnDraft::Kept);
            }
            // Gone from here now, with nothing queued that would look for
            // its copy by Message-ID: that copy goes below, by its UID.
            store.remove_messages(&[id])?;
        }
        let set = uid.to_string();
        let deleted: async_imap::error::Result<()> = async {
            let _: Vec<Fetch> = session
                .uid_store(&set, "+FLAGS.SILENT (\\Deleted)")
                .await?
                .try_collect()
                .await?;
            let _: Vec<_> = session.uid_expunge(&set).await?.try_collect().await?;
            Ok(())
        }
        .await;
        // Left on the server, it comes back as a draft of the user's.
        if let Err(e) = deleted {
            eprintln!("spark-rs: couldn't delete an auto-draft's copy on the server: {e}");
        }
        Ok(OwnDraft::Deleted)
    }

    /// Sends the account's drafts whose time has come.
    pub async fn send_due(&self, account_id: i64) -> Result<Vec<SendOutcome>> {
        self.send_due_each(account_id, |_| {}).await
    }

    /// `send_due`, telling `each` how each send went as soon as it's done:
    /// a reply that's gone is answered then, not once the others are too.
    pub async fn send_due_each(
        &self,
        account_id: i64,
        mut each: impl FnMut(&SendOutcome),
    ) -> Result<Vec<SendOutcome>> {
        let due = self.store().claim_due_sends(account_id, now_ms())?;
        let mut outcomes = Vec::new();
        for (id, json) in due {
            let parsed = serde_json::from_str::<Draft>(&json);
            let saved_draft = parsed.as_ref().ok().and_then(|d| d.draft_id);
            let reply_to = parsed
                .as_ref()
                .ok()
                .and_then(|d| self.answers(account_id, d));
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
            let outcome = SendOutcome {
                id,
                draft_id,
                reply_to,
                result,
            };
            each(&outcome);
            outcomes.push(outcome);
        }
        Ok(outcomes)
    }

    /// The stored email `draft` replies to: the one it quotes, or, for one
    /// written elsewhere with the quote in its text already, the one its
    /// In-Reply-To names.
    fn answers(&self, account_id: i64, draft: &Draft) -> Option<i64> {
        if draft.reply_to.is_some() {
            return draft.reply_to;
        }
        let original = draft.in_reply_to.as_deref()?;
        let store = self.store();
        let bare = crate::parse::bare_id(original);
        [bare.as_str(), original]
            .into_iter()
            .find_map(|id| store.find_message_id(account_id, id).ok().flatten())
            .filter(|found| Some(*found) != draft.draft_id)
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
        // An IMAP reply quotes the email it answers: one stored without its
        // body gets it first, if its server gives it.
        if let Some(id) = draft.reply_to
            && matches!(config(&account)?, AccountConfig::Imap { .. })
        {
            let _ = self.fetch_body(id).await;
        }
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

    /// Makes an event in the user's calendar `draft.calendar`, written in
    /// `zone`; Outlook sends the invitations. The calendar's next refresh
    /// brings it in.
    pub async fn create_event(&self, draft: &EventDraft, zone: Zone<'_>) -> Result<()> {
        let calendar = self
            .store()
            .calendars()?
            .into_iter()
            .find(|c| c.id == draft.calendar)
            .ok_or_else(|| Error::Config("That calendar is no longer here.".into()))?;
        let graph = self.calendar_graph(calendar.account_id).await?;
        graph
            .create_event(&calendar.remote_id, &calendar::new_event_body(draft, zone))
            .await?;
        Ok(())
    }

    /// Changes the user's stored event `event_id` to say what `draft` says:
    /// that occurrence, with `Scope::All` every occurrence of its series,
    /// which move by as much as this one did, or with `Scope::Future` this
    /// one and those after it (`split_series`). Only what changed from
    /// `before`, the event as the editor opened it (else as stored), is
    /// sent, so what changed in Outlook meanwhile stays. Outlook sends the
    /// people invited the update.
    pub async fn update_event(
        &self,
        event_id: i64,
        draft: &EventDraft,
        before: Option<&EventDraft>,
        scope: Scope,
        zone: Zone<'_>,
    ) -> Result<()> {
        if scope == Scope::Future {
            return self.split_series(event_id, draft, before, zone).await;
        }
        self.update_scoped(event_id, draft, before, scope, zone)
            .await
    }

    async fn update_scoped(
        &self,
        event_id: i64,
        draft: &EventDraft,
        before: Option<&EventDraft>,
        scope: Scope,
        zone: Zone<'_>,
    ) -> Result<()> {
        let (target, event, graph) = self.event_target(event_id, scope).await?;
        let mut details = graph.event_details(&target).await?;
        let series = scope == Scope::All && event.series.as_deref() == Some(target.as_str());
        let opened = EventDraft::of(&event, draft.calendar);
        let before = before.unwrap_or(&opened);
        // A series is written on its own clock, where its days are.
        let clock = if series {
            let stored = self
                .store()
                .event(event_id)?
                .ok_or_else(|| Error::Config("That event is no longer here.".into()))?;
            let master = serde_json::json!({ "recurrence": details.recurrence });
            Some(series_clock(&graph, &stored.remote_id, stored.start, &master, zone).await?)
        } else {
            None
        };
        let zone = clock.as_ref().map_or(zone, |c| c.zone(zone));
        // A monthly or yearly series moved to another date lines up from its
        // first occurrence, which can be weeks after its range starts (the
        // 15th, from January 30).
        let day = |t: i64, all_day: bool| calendar::date_in(zone, t, all_day);
        if series
            && recurrence::by_date(&details.recurrence)
            && day(draft.start, draft.all_day) != day(before.start, before.all_day)
        {
            let range = details.recurrence["range"]["startDate"].as_str();
            if let Some(from) = range.and_then(|d| graph::parse_iso_date(&format!("{d}T00:00:00Z")))
            {
                details.first = graph
                    .occurrence_span(
                        &target,
                        from - 86_400_000,
                        before.start + 2 * 86_400_000,
                        zone.name,
                    )
                    .await?
                    .map(|(first, _)| first);
            }
        }
        let body = calendar::changed_event_body(draft, before, &details, zone, series);
        if body.as_object().is_some_and(|b| b.is_empty()) {
            return Ok(());
        }
        graph.update_event(&target, &body).await
    }

    /// "All future": a new series from occurrence `event_id` on, saying
    /// what `draft` says (`calendar::series_from`), and its old series
    /// ending the day before, that day counted in the series' own time
    /// zone. A series' first occurrence changes the whole series instead.
    /// The new series' days move as the occurrence did
    /// (`recurrence::changed`). One ending after so many times ends on the
    /// day the old one would have, unless its end changed. The old series
    /// ends first, and goes on as it was if the new one can't be made.
    async fn split_series(
        &self,
        event_id: i64,
        draft: &EventDraft,
        before: Option<&EventDraft>,
        zone: Zone<'_>,
    ) -> Result<()> {
        let stored = self
            .store()
            .event(event_id)?
            .ok_or_else(|| Error::Config("That event is no longer here.".into()))?;
        let event = Event::from_json(&stored.data);
        let Some(series) = event.series.clone() else {
            return self
                .update_scoped(event_id, draft, before, Scope::One, zone)
                .await;
        };
        let opened = EventDraft::of(&event, draft.calendar);
        let before = before.unwrap_or(&opened);
        let graph = self.calendar_graph(stored.account_id).await?;
        let master = graph.event_json(&series, calendar::MASTER_SELECT).await?;
        let old = master["recurrence"].clone();
        let clock = series_clock(&graph, &stored.remote_id, stored.start, &master, zone).await?;
        let split = clock.day.clone();
        if begins_on(&graph, &series, &old, &split, &clock.name).await? {
            return self
                .update_scoped(event_id, draft, Some(before), Scope::All, zone)
                .await;
        }
        // From here on, the series' own clock: the new series is written
        // there, as the old one was, so its days and dates mean the same.
        let zone = clock.zone(zone);
        let rule = if draft.repeat != before.repeat {
            draft.repeat.as_ref()
        } else {
            None
        };
        // A monthly or yearly rule the editor can't show ("the last weekday")
        // keeps its days, so the new series does too, from where this one is.
        let kept =
            rule.is_none() && recurrence::by_date(&old) && Repeat::from_graph(&old).is_none();
        // The day it goes to there, and how far the user moved it. One not
        // moved here stays on the day Outlook has it now, which may be
        // another than the copy here says (made all-day there since). An
        // all-day one goes by as many days from where it is on the series'
        // clock: its date here is UTC's, a day off for a series 13 hours from
        // it.
        let (from, moved) =
            if kept || (draft.start, draft.all_day) == (before.start, before.all_day) {
                (clock.current.clone(), 0)
            } else if draft.all_day && before.all_day {
                let moved = (draft.start - before.start).div_euclid(86_400_000);
                (recurrence::add_days(&clock.current, moved), moved)
            } else {
                let from = calendar::date_in(zone, draft.start, draft.all_day);
                let moved = recurrence::days_between(
                    &calendar::date_in(zone, before.start, before.all_day),
                    &from,
                );
                (from, moved)
            };
        let days = recurrence::days_between(&clock.current, &from);
        let mut new = recurrence::changed(
            &old,
            rule,
            before.repeat.as_ref(),
            days,
            Some((&from, zone.name)),
            &from,
        );
        // Read from Outlook's own range, so a rule the editor doesn't know
        // counts too.
        // Its end is the user's only if they changed it from the one the
        // editor opened with; else it's Outlook's, which may have changed.
        let opened = before.repeat.clone().or_else(|| Repeat::from_graph(&old));
        let ends_kept = rule.is_none_or(|r| opened.as_ref().is_some_and(|w| w.ends == r.ends));
        if ends_kept && old["range"]["type"] == "numbered" {
            let times = old["range"]["numberOfOccurrences"]
                .as_i64()
                .unwrap_or(1)
                .max(1);
            let interval = old["pattern"]["interval"].as_i64().unwrap_or(1).max(1);
            let period = match old["pattern"]["type"].as_str() {
                Some("daily") => 1,
                Some("weekly") => 7,
                Some("absoluteMonthly" | "relativeMonthly") => 31,
                _ => 366,
            };
            let span = (interval * times * period + 366) * 86_400_000;
            let from_ms = stored.start - 86_400_000;
            if let Some((_, last)) = graph
                .occurrence_span(&series, from_ms, from_ms + span, zone.name)
                .await?
            {
                new = recurrence::ending_on(&new, &recurrence::add_days(&last, moved));
            }
        }
        let calendar = self
            .store()
            .calendars()?
            .into_iter()
            .find(|c| c.id == stored.calendar_id)
            .ok_or_else(|| Error::Config("That calendar is no longer here.".into()))?;
        let mut body = calendar::series_from(&master, draft, before, zone, &from, new);
        // Made once, however often it's sent: Graph knows a new series it
        // already made by this.
        let mut id = [0u8; 16];
        getrandom::fill(&mut id).map_err(|e| Error::Config(e.to_string()))?;
        body["transactionId"] =
            serde_json::json!(id.iter().map(|b| format!("{b:02x}")).collect::<String>());
        // The old series ends first, so a room it booked is free for the new
        // one's invitation; if the new one can't be made, the old one goes
        // on as it was.
        let ended = serde_json::json!({ "recurrence": recurrence::ended_before(&old, &split) });
        if let Err(e) = graph.update_event(&series, &ended).await {
            // Its answer may be all that was lost: if the series ends there
            // now, it was ended. If that can't be told, it's put back as it
            // was (which changes nothing if it wasn't ended).
            let range = &ended["recurrence"]["range"];
            match graph.event_json(&series, "recurrence").await {
                Ok(m)
                    if m["recurrence"]["range"]["type"] == "endDate"
                        && m["recurrence"]["range"]["endDate"] == range["endDate"] => {}
                Ok(_) => return Err(e),
                Err(_) => {
                    if put_back(&graph, &series, &old).await {
                        return Err(e);
                    }
                    return Err(Error::Config(format!(
                        "Couldn't change the series ({e}), and couldn't tell whether its \
                         meetings from {split} on are still there. Check it in Outlook."
                    )));
                }
            }
        }
        let mut made = Err(Error::Config("The new series wasn't made.".into()));
        for wait in [0, 10, 30] {
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            made = graph.create_event(&calendar.remote_id, &body).await;
            if made.is_ok() {
                break;
            }
        }
        let created = match made {
            Ok(created) => created,
            Err(e) => {
                // Put back as it was; if that fails too, say what's gone.
                if put_back(&graph, &series, &old).await {
                    return Err(e);
                }
                return Err(Error::Config(format!(
                    "Couldn't make the new series ({e}), nor put the old one back: its meetings from \
                     {split} on are gone. Put them back in Outlook."
                )));
            }
        };
        // Its files (an agenda) go with it, those Graph takes in one go (up
        // to 3 MB each); an update then sends them to the people invited.
        if master["hasAttachments"] == true {
            let copied = async {
                let mut any = false;
                for file in graph.event_attachments(&series).await? {
                    let small = file["size"].as_u64().is_some_and(|s| s <= 3_000_000);
                    if file["@odata.type"] == "#microsoft.graph.fileAttachment"
                        && small
                        && file["contentBytes"].is_string()
                    {
                        let copy = serde_json::json!({
                            "@odata.type": "#microsoft.graph.fileAttachment",
                            "name": file["name"],
                            "contentType": file["contentType"],
                            "contentBytes": file["contentBytes"],
                            "isInline": file["isInline"],
                            "contentId": file["contentId"],
                        });
                        graph.add_attachment(&created, &copy).await?;
                        any = true;
                    }
                }
                if any {
                    let subject = serde_json::json!({ "subject": body["subject"] });
                    graph.update_event(&created, &subject).await?;
                }
                Ok::<_, Error>(())
            }
            .await;
            if let Err(e) = copied {
                return Err(Error::Config(format!(
                    "The meetings from {split} on are changed, but their files didn't all \
                     reach them ({e}). Add them in Outlook."
                )));
            }
        }
        Ok(())
    }

    /// How stored event `event_id`'s series repeats, if it's an occurrence
    /// of one and the editor knows its rule.
    pub async fn event_repeat(&self, event_id: i64) -> Result<Option<Repeat>> {
        let stored = self
            .store()
            .event(event_id)?
            .ok_or_else(|| Error::Config("That event is no longer here.".into()))?;
        let Some(series) = Event::from_json(&stored.data).series else {
            return Ok(None);
        };
        let graph = self.calendar_graph(stored.account_id).await?;
        let master = graph.event_json(&series, "recurrence").await?;
        Ok(Repeat::from_graph(&master["recurrence"]))
    }

    /// Deletes the user's stored event `event_id`: that occurrence, with
    /// `Scope::All` its whole series, or with `Scope::Future` the series
    /// from this occurrence on (it ends the day before, in its own time
    /// zone; all of it, if this is its first). The people invited hear it's
    /// canceled.
    pub async fn delete_event(&self, event_id: i64, scope: Scope, zone: Zone<'_>) -> Result<()> {
        if scope != Scope::Future {
            let (target, _, graph) = self.event_target(event_id, scope).await?;
            return graph.delete_event(&target).await;
        }
        let stored = self
            .store()
            .event(event_id)?
            .ok_or_else(|| Error::Config("That event is no longer here.".into()))?;
        let graph = self.calendar_graph(stored.account_id).await?;
        let Some(series) = Event::from_json(&stored.data).series else {
            return graph.delete_event(&stored.remote_id).await;
        };
        let master = graph
            .event_json(&series, "recurrence,originalStartTimeZone")
            .await?;
        let old = &master["recurrence"];
        let clock = series_clock(&graph, &stored.remote_id, stored.start, &master, zone).await?;
        let split = clock.day;
        if old.is_null() || begins_on(&graph, &series, old, &split, &clock.name).await? {
            return graph.delete_event(&series).await;
        }
        let ended = serde_json::json!({ "recurrence": recurrence::ended_before(old, &split) });
        graph.update_event(&series, &ended).await
    }

    /// What a change to stored event `event_id` goes to on the server (the
    /// occurrence, or for `Scope::All` its series), the event, and its
    /// account's Graph.
    async fn event_target(&self, event_id: i64, scope: Scope) -> Result<(String, Event, Graph)> {
        let stored = self
            .store()
            .event(event_id)?
            .ok_or_else(|| Error::Config("That event is no longer here.".into()))?;
        let event = Event::from_json(&stored.data);
        let target = match (scope, &event.series) {
            (Scope::All, Some(series)) => series.clone(),
            _ => stored.remote_id.clone(),
        };
        let graph = self.calendar_graph(stored.account_id).await?;
        Ok((target, event, graph))
    }

    /// Graph for changing the calendar of `account_id`, a Microsoft 365
    /// account's.
    async fn calendar_graph(&self, account_id: i64) -> Result<Graph> {
        let account = self.account(account_id)?;
        let AccountConfig::Microsoft {
            client_id, tenant, ..
        } = config(&account)?
        else {
            return Err(Error::Config(
                "Only Microsoft 365 calendars can be changed here.".into(),
            ));
        };
        self.graph(account.id, &client_id, &tenant).await
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

    fn paces_lock(&self) -> MutexGuard<'_, HashMap<i64, Pace>> {
        self.paces
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn bodies_lock(&self) -> MutexGuard<'_, HashMap<i64, Arc<AsyncMutex<()>>>> {
        self.bodies
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Where the background download's window starts, at `now` (Unix ms).
fn backfill_since(now: i64) -> i64 {
    now - BACKFILL_DAYS * 86_400_000
}

/// A series' own clock: its time zone, where its range's dates are, which
/// can be another than the user's.
struct SeriesClock {
    name: String,
    /// Minutes it's ahead of the user's clock, at the occurrence it was
    /// read from.
    shift: i64,
    /// The day ("2026-10-27") that occurrence is on there.
    current: String,
    /// The day its place in the series is: where it was before it was moved
    /// on its own ("Only This Event"), else `current`.
    day: String,
}

impl SeriesClock {
    /// The user's `zone`, told on this clock.
    fn zone<'a>(&'a self, zone: Zone<'_>) -> Zone<'a> {
        Zone {
            name: &self.name,
            wall: zone.wall,
            shift: self.shift,
        }
    }
}

/// Series `master`'s clock, read from its occurrence `id`, which starts at
/// `start` (Unix ms; an all-day one's date), against the user's `zone`, and
/// where in the series that occurrence is.
async fn series_clock(
    graph: &Graph,
    id: &str,
    start: i64,
    master: &serde_json::Value,
    zone: Zone<'_>,
) -> Result<SeriesClock> {
    let name = master["recurrence"]["range"]["recurrenceTimeZone"]
        .as_str()
        .or_else(|| master["originalStartTimeZone"].as_str())
        .filter(|z| !z.is_empty())
        .unwrap_or(zone.name)
        .to_owned();
    // Where the occurrence is now, on this clock and in UTC, and whether
    // it's all-day, all from Graph: how far apart the clocks are comes from
    // the same moment, not from the copy stored here, which Outlook may have
    // moved or made all-day (or not) since.
    let (wall, original, all_day) = graph.start_in(id, &name).await?;
    let (utc, _, _) = graph.start_in(id, "UTC").await?;
    let current = wall.get(..10).unwrap_or_default().to_owned();
    let naive = |w: &str| graph::parse_iso_date(&format!("{}Z", w.get(..19).unwrap_or(w)));
    let now = naive(&utc).unwrap_or(start);
    let shift = if all_day || name == zone.name {
        0
    } else {
        match (naive(&wall), naive(&(zone.wall)(now))) {
            (Some(there), Some(here)) => (there - here) / 60_000,
            _ => 0,
        }
    };
    // Moved on its own: the day it was to be on, on this clock, from its
    // original start (UTC's).
    let day = match (original, naive(&wall)) {
        (Some(original), Some(there)) => calendar::utc_wall(original + (there - now)),
        _ => wall,
    }
    .get(..10)
    .unwrap_or_default()
    .to_owned();
    Ok(SeriesClock {
        name,
        shift,
        current,
        day,
    })
}

/// Puts series `series` back to repeating by `old` (Graph's recurrence),
/// waiting out Graph's pauses for a couple of minutes. Whether it took.
async fn put_back(graph: &Graph, series: &str, old: &serde_json::Value) -> bool {
    let restore = serde_json::json!({ "recurrence": old });
    for wait in [0, 10, 30, 60] {
        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
        if graph.update_event(series, &restore).await.is_ok() {
            return true;
        }
    }
    false
}

/// Whether series `series` (Graph's `old` recurrence) begins on `split`
/// ("2026-10-27"), its own time zone `zone`'s day: on or before its range's
/// start, or before any occurrence it has, as a range can start days before
/// the first day its pattern has.
async fn begins_on(
    graph: &Graph,
    series: &str,
    old: &serde_json::Value,
    split: &str,
    zone: &str,
) -> Result<bool> {
    let first = old["range"]["startDate"].as_str().unwrap_or_default();
    if split.is_empty() || split <= first {
        return Ok(true);
    }
    let day = |d: &str| graph::parse_iso_date(&format!("{d}T00:00:00Z"));
    let (Some(from), Some(to)) = (day(first), day(split)) else {
        return Ok(false);
    };
    let span = graph
        .occurrence_span(series, from - 86_400_000, to + 86_400_000, zone)
        .await?;
    Ok(span.is_none_or(|(earliest, _)| earliest.as_str() >= split))
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
    use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering::SeqCst};

    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use spark_store::{Added, Attachment, Body, FolderRole, Incoming, OutboxFile, RemoteFolder};

    use crate::graph::tests::{Answer, Asked};
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
            smtp_session(socket, code).await;
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

    /// One SMTP conversation that takes a message, answering RCPT with `code`.
    async fn smtp_session(socket: TcpStream, code: u16) {
        {
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
        }
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
    async fn a_sent_reply_says_what_it_answered() {
        let (smtp, delivery) = smtp_delivery(250).await;
        let mut store = Store::open_in_memory().unwrap();
        let account = test_account(&mut store, smtp);
        let Added::New(original) = store
            .add_incoming(
                &Incoming {
                    account_id: account.id,
                    mailbox: Some("INBOX".into()),
                    uid: Some(1),
                    message_id: Some("<asked@example.com>".into()),
                    from_address: "recipient@example.com".into(),
                    subject: "Lunch?".into(),
                    ..Default::default()
                },
                false,
            )
            .unwrap()
        else {
            panic!("new");
        };
        let draft = Draft {
            account: account.id,
            to: "recipient@example.com".into(),
            subject: "Re: Lunch?".into(),
            body: "Yes.".into(),
            reply_to: Some(original),
            ..Default::default()
        };
        store
            .queue_send(account.id, &to_json(&draft), 0, &[])
            .unwrap();
        let engine = Engine::new(Arc::new(Mutex::new(store)), Arc::new(TestSecrets));
        let outcomes = engine.send_due(account.id).await.unwrap();
        assert!(matches!(outcomes[0].result, Ok(true)));
        assert_eq!(outcomes[0].reply_to, Some(original));
        delivery.await.unwrap();
    }

    #[tokio::test]
    async fn a_reply_written_elsewhere_says_what_it_answered_too() {
        let (smtp, delivery) = smtp_delivery(250).await;
        let mut store = Store::open_in_memory().unwrap();
        let account = test_account(&mut store, smtp);
        let Added::New(original) = store
            .add_incoming(
                &Incoming {
                    account_id: account.id,
                    mailbox: Some("INBOX".into()),
                    uid: Some(1),
                    message_id: Some("asked@example.com".into()),
                    from_address: "recipient@example.com".into(),
                    subject: "Lunch?".into(),
                    ..Default::default()
                },
                false,
            )
            .unwrap()
        else {
            panic!("new");
        };
        // Opened from Drafts with its quote in its text: it only threads.
        let draft = Draft {
            account: account.id,
            to: "recipient@example.com".into(),
            subject: "Re: Lunch?".into(),
            body: "Yes.\n\n> Lunch?".into(),
            in_reply_to: Some("<asked@example.com>".into()),
            ..Default::default()
        };
        store
            .queue_send(account.id, &to_json(&draft), 0, &[])
            .unwrap();
        let engine = Engine::new(Arc::new(Mutex::new(store)), Arc::new(TestSecrets));
        let outcomes = engine.send_due(account.id).await.unwrap();
        assert!(matches!(outcomes[0].result, Ok(true)));
        assert_eq!(outcomes[0].reply_to, Some(original));
        delivery.await.unwrap();
    }

    #[tokio::test]
    async fn each_send_is_heard_as_soon_as_its_done() {
        // The second send's server greets it only once the first is heard.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let smtp = Server {
            host: "127.0.0.1".into(),
            port: listener.local_addr().unwrap().port(),
            security: imap::Security::Plain,
            trusted_cert: None,
        };
        let (go, gate) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let (first, _) = listener.accept().await.unwrap();
            smtp_session(first, 250).await;
            let (second, _) = listener.accept().await.unwrap();
            let _ = gate.await;
            smtp_session(second, 250).await;
        });
        let mut store = Store::open_in_memory().unwrap();
        let account = test_account(&mut store, smtp);
        for subject in ["Re: Lunch?", "Re: Dinner?"] {
            let draft = Draft {
                account: account.id,
                to: "recipient@example.com".into(),
                subject: subject.into(),
                body: "Yes.".into(),
                ..Default::default()
            };
            store
                .queue_send(account.id, &to_json(&draft), 0, &[])
                .unwrap();
        }
        let engine = Engine::new(Arc::new(Mutex::new(store)), Arc::new(TestSecrets));
        let mut go = Some(go);
        let mut heard = 0;
        let sent = engine.send_due_each(account.id, |outcome| {
            assert!(matches!(outcome.result, Ok(true)));
            heard += 1;
            if let Some(go) = go.take() {
                let _ = go.send(());
            }
        });
        let outcomes = tokio::time::timeout(std::time::Duration::from_secs(10), sent)
            .await
            .expect("the first send was heard before the second went")
            .unwrap();
        assert_eq!((outcomes.len(), heard), (2, 2));
        server.await.unwrap();
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

    /// The header fields of `raw` named in `fields` ("FROM SUBJECT"), as
    /// a FETCH of them answers.
    fn header_fields(raw: &[u8], fields: &str) -> Vec<u8> {
        let text = String::from_utf8_lossy(raw);
        let header = text.split("\r\n\r\n").next().unwrap_or_default();
        let mut data = String::new();
        for line in header.split("\r\n") {
            if let Some((name, _)) = line.split_once(':')
                && fields
                    .split_whitespace()
                    .any(|f| f.eq_ignore_ascii_case(name.trim()))
            {
                data.push_str(line);
                data.push_str("\r\n");
            }
        }
        data.push_str("\r\n");
        data.into_bytes()
    }

    /// The INTERNALDATE of `raw`: its Date, else the start of 1970.
    fn internal_date_of(raw: &[u8]) -> String {
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        let secs = mail_parser::MessageParser::default()
            .parse_headers(raw)
            .and_then(|m| m.date().map(|d| d.to_timestamp()))
            .unwrap_or(0);
        let (y, m, d) = graph::civil_from_days(secs.div_euclid(86_400));
        let t = secs.rem_euclid(86_400);
        format!(
            "{d:02}-{}-{y} {:02}:{:02}:{:02} +0000",
            MONTHS[(m - 1) as usize],
            t / 3600,
            t % 3600 / 60,
            t % 60
        )
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
            } else if upper.starts_with("SELECT ") || upper.starts_with("EXAMINE ") {
                selected = command
                    .split_once(' ')
                    .unwrap()
                    .1
                    .trim_matches('"')
                    .to_owned();
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
                // A UID set of single UIDs and ranges.
                let set = command.split_whitespace().nth(2).unwrap().to_owned();
                let wanted = |uid: u32| {
                    set.split(',').any(|r| match r.split_once(':') {
                        Some((low, high)) => {
                            (low.parse().unwrap()..=high.parse().unwrap_or(u32::MAX)).contains(&uid)
                        }
                        None => r.parse() == Ok(uid),
                    })
                };
                let found: Vec<(usize, u32, Vec<u8>)> = held.lock().unwrap()[&selected]
                    .1
                    .iter()
                    .enumerate()
                    .filter(|(_, (u, _))| wanted(*u))
                    .map(|(i, (u, raw))| (i + 1, *u, raw.clone()))
                    .collect();
                for (seq, uid, raw) in found {
                    // The fields asked for, to check a search's hits, and
                    // its arrival, as its Date says (else 1970).
                    let data = if let Some((_, fields)) = command.split_once("HEADER.FIELDS (") {
                        let fields = fields.split(')').next().unwrap();
                        let data = header_fields(&raw, fields);
                        let arrived = if upper.contains("INTERNALDATE") {
                            format!("INTERNALDATE \"{}\" ", internal_date_of(&raw))
                        } else {
                            String::new()
                        };
                        let head = format!(
                            "* {seq} FETCH (UID {uid} {arrived}BODY[HEADER.FIELDS ({fields})] {{{}}}\r\n",
                            data.len()
                        );
                        write.write_all(head.as_bytes()).await?;
                        data
                    } else {
                        fetches.fetch_add(1, SeqCst);
                        let head = format!("* {seq} FETCH (UID {uid} BODY[] {{{}}}\r\n", raw.len());
                        write.write_all(head.as_bytes()).await?;
                        raw
                    };
                    write.write_all(&data).await?;
                    write.write_all(b")\r\n").await?;
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

    /// Stores `raw` as UID `uid` of the inbox without its body, as the
    /// background download would.
    fn store_head(store: &mut Store, account_id: i64, uid: u32, raw: &[u8]) -> i64 {
        let parsed = mail_parser::MessageParser::default().parse(raw).unwrap();
        let incoming = Incoming {
            account_id,
            mailbox: Some(imap::INBOX.into()),
            uid: Some(uid),
            message_id: parsed.message_id().map(str::to_owned),
            from_address: "ana@example.com".into(),
            subject: "Files".into(),
            snippet: "The files".into(),
            text: "The files".into(),
            date: 1,
            attachments: vec![Attachment {
                name: "part0.pdf".into(),
                ..Default::default()
            }],
            body: Body::Missing,
            ..Default::default()
        };
        match store.add_incoming(&incoming, false).unwrap() {
            Added::New(id) | Added::Known(id) => id,
        }
    }

    #[tokio::test]
    async fn opening_older_mail_downloads_its_body_once_with_its_files() {
        const SIZE: usize = 16 * 1024;
        let (one, two) = (
            big_message("one@example.com", 1, SIZE),
            big_message("two@example.com", 2, SIZE),
        );
        let imap = ImapStandIn::start(7, vec![(42, one.clone()), (43, two.clone())]).await;
        let mut store = Store::open_in_memory().unwrap();
        let account = imap_account(&store, &imap.server, 7);
        let a = store_head(&mut store, account.id, 42, &one);
        let b = store_head(&mut store, account.id, 43, &two);
        let gone = store_head(
            &mut store,
            account.id,
            44,
            b"Message-ID: <gone@example.com>\r\n\r\n",
        );
        let dir = files_dir("bodies");
        let store = Arc::new(Mutex::new(store));
        let engine = caching(store.clone(), &dir);

        // The reader and a reply ask at once: one download.
        let (x, y) = tokio::join!(engine.fetch_body(a), engine.fetch_body(a));
        assert_eq!((x.unwrap(), y.unwrap()), (true, false));
        assert_eq!(imap.counts(), (1, 1));
        let content = store.lock().unwrap().content(a).unwrap().unwrap();
        assert!(!content.body_missing);
        assert_eq!(content.html.as_deref(), Some(HTML));
        assert_eq!(content.attachments.len(), 4);
        assert!(engine.bodies_lock().is_empty(), "nothing left behind");

        // Its files and images came with it: opening them, or Auto-download,
        // downloads nothing more.
        assert_eq!(engine.prefetch_attachments(a).await.unwrap(), 0);
        assert_eq!(
            engine.attachment(a, 2).await.unwrap().bytes,
            part(1, 2, SIZE)
        );
        assert_eq!(engine.inline_images(a).await.unwrap().len(), 2);
        assert_eq!(imap.counts(), (1, 1));
        // Asked again, it has it: no connection.
        assert!(!engine.fetch_body(a).await.unwrap());
        assert_eq!(imap.counts(), (1, 1));

        // Without a cache, it still fills in.
        let bare = Engine::new(store.clone(), Arc::new(TestSecrets));
        assert!(bare.fetch_body(b).await.unwrap());
        assert_eq!(imap.counts(), (2, 2));

        // Gone from the server: it fails, and still waits for its body.
        assert!(matches!(engine.fetch_body(gone).await, Err(Error::Gone)));
        assert!(
            store
                .lock()
                .unwrap()
                .content(gone)
                .unwrap()
                .unwrap()
                .body_missing
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn opening_older_mail_never_fills_in_another_emails_body() {
        // UID 42 is gone, and the Message-ID stored for it is part of 142's.
        let other = big_message("142@example.com", 2, 1024);
        let imap = ImapStandIn::start(7, vec![(142, other.clone())]).await;
        let mut store = Store::open_in_memory().unwrap();
        let account = imap_account(&store, &imap.server, 7);
        let id = store_head(
            &mut store,
            account.id,
            42,
            b"Message-ID: <42@example.com>\r\n\r\n",
        );
        let store = Arc::new(Mutex::new(store));
        let engine = Engine::new(store.clone(), Arc::new(TestSecrets));
        let fetched = engine.fetch_body(id).await;
        assert!(matches!(fetched, Err(Error::Gone)), "{fetched:?}");
        let content = store.lock().unwrap().content(id).unwrap().unwrap();
        assert!(content.body_missing && content.html.is_none());
        assert_eq!(content.attachments.len(), 1);

        // Nor what a UID that names some other email now holds.
        imap.hold(imap::INBOX, 7, vec![(42, other.clone())]);
        assert!(engine.fetch_body(id).await.is_err());
        assert!(
            store
                .lock()
                .unwrap()
                .content(id)
                .unwrap()
                .unwrap()
                .body_missing
        );

        // The email itself, found by Message-ID: filled in.
        let mine = big_message("42@example.com", 1, 1024);
        imap.hold(imap::INBOX, 7, vec![(42, other), (50, mine)]);
        assert!(engine.fetch_body(id).await.unwrap());
        let content = store.lock().unwrap().content(id).unwrap().unwrap();
        assert_eq!(content.html.as_deref(), Some(HTML));
    }

    #[tokio::test]
    async fn opening_older_mail_takes_it_and_not_a_changed_copy_sent_again() {
        // Stored without its body at UID 42, which is gone: it was moved
        // away and back (50), and a corrected copy with its Message-ID came
        // after (51), as stored but for when it came and what it says.
        let email = |date: &str, text: &str| {
            format!(
                "From: ana@example.com\r\nSubject: Files\r\nMessage-ID: <dup@example.com>\r\n\
                 {date}Content-Type: text/plain\r\n\r\n{text}\r\n"
            )
            .into_bytes()
        };
        let original = email("", "The figures.");
        let resent = email(
            "Date: Tue, 21 Oct 2025 14:00:00 +0000\r\n",
            "The corrected figures.",
        );
        let imap = ImapStandIn::start(7, vec![(50, original.clone()), (51, resent.clone())]).await;
        let mut store = Store::open_in_memory().unwrap();
        let account = imap_account(&store, &imap.server, 7);
        // Stored as it came, at the start of 1970 (`store_head`).
        let id = store_head(&mut store, account.id, 42, &original);
        let store = Arc::new(Mutex::new(store));
        let engine = Engine::new(store.clone(), Arc::new(TestSecrets));
        let content = |store: &Mutex<Store>| store.lock().unwrap().content(id).unwrap().unwrap();

        // Two copies alike, that came at the same time: which can't be told.
        imap.hold(
            imap::INBOX,
            7,
            vec![(50, original.clone()), (52, original.clone())],
        );
        let fetched = engine.fetch_body(id).await;
        assert!(matches!(&fetched, Err(Error::Server(_))), "{fetched:?}");
        assert!(content(&store).body_missing);

        // Only the copy sent again, its subject changed: not it either.
        let renamed = String::from_utf8(resent.clone())
            .unwrap()
            .replace("Subject: Files", "Subject: Files (corrected)")
            .into_bytes();
        imap.hold(imap::INBOX, 7, vec![(51, renamed)]);
        let fetched = engine.fetch_body(id).await;
        assert!(
            matches!(&fetched, Err(Error::Server(e)) if e == imap::NOT_THE_ONE),
            "{fetched:?}"
        );
        assert!(content(&store).body_missing);

        // The one that came when it did.
        imap.hold(imap::INBOX, 7, vec![(50, original), (51, resent)]);
        assert!(engine.fetch_body(id).await.unwrap());
        assert_eq!(content(&store).text.trim(), "The figures.");
    }

    #[tokio::test]
    async fn what_a_round_couldnt_push_doesnt_hold_up_the_background_download() {
        let raw = b"Message-ID: <a@x>\r\n\r\n".to_vec();
        let imap = ImapStandIn::start(7, vec![(42, raw.clone())]).await;
        let mut store = Store::open_in_memory().unwrap();
        let account = imap_account(&store, &imap.server, 7);
        let id = store_imap(&mut store, account.id, imap::INBOX, 42, &raw);
        // Older mail is left to download; there's an Archive to move to.
        let state = imap::SyncState {
            uid_validity: Some(7),
            floor: Some(500),
            window: Some(100),
            archive: Some("Archive".into()),
            trash: Some("Trash".into()),
            ..Default::default()
        };
        store.set_sync_state(account.id, &to_json(&state)).unwrap();
        // Done, which this server can't move (no MOVE or UIDPLUS).
        let done = spark_store::Patch {
            state: Some(spark_store::MessageState::Done),
            ..Default::default()
        };
        store.update(id, &done, now_ms() - 60_000).unwrap();
        let store = Arc::new(Mutex::new(store));
        let engine = Engine::new(store.clone(), Arc::new(TestSecrets));
        assert!(!engine.backfill_due(account.id), "till a round tries it");

        let round = engine.run(account.id, false).await;
        assert!(matches!(round, Err(Error::Server(_))), "{round:?}");
        let pending = |store: &Mutex<Store>| {
            store
                .lock()
                .unwrap()
                .pending(account.id, i64::MAX, 0)
                .unwrap()
                .len()
        };
        assert_eq!(pending(&store), 1, "it waits to be pushed");
        assert!(engine.backfill_due(account.id), "for the next round");

        // A change made since waits for a round, which tries that too.
        let other = store_imap(
            &mut store.lock().unwrap(),
            account.id,
            imap::INBOX,
            43,
            b"Message-ID: <b@x>\r\n\r\n",
        );
        let since_tried = || engine.paces_lock()[&account.id].tried.changes.unwrap() + 1;
        let pin = spark_store::Patch {
            pinned: Some(true),
            ..Default::default()
        };
        let at = since_tried();
        store.lock().unwrap().update(other, &pin, at).unwrap();
        assert!(!engine.backfill_due(account.id));
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert!(engine.run(account.id, false).await.is_err());
        assert!(engine.backfill_due(account.id));

        // Not a round that didn't reach the server.
        let third = store_imap(
            &mut store.lock().unwrap(),
            account.id,
            imap::INBOX,
            44,
            b"Message-ID: <c@x>\r\n\r\n",
        );
        let at = since_tried();
        store.lock().unwrap().update(third, &pin, at).unwrap();
        drop(imap);
        let round = engine.run(account.id, false).await;
        assert!(matches!(round, Err(Error::Network { .. })), "{round:?}");
        assert!(!engine.backfill_due(account.id));
    }

    #[tokio::test]
    async fn older_microsoft_mail_is_gone_over_after_start_and_hourly() {
        fn empty(_: &str) -> Option<serde_json::Value> {
            Some(serde_json::json!({ "value": [] }))
        }
        let (base, seen) = graph::tests::stand_in(empty).await;
        let (engine, account_id) = microsoft_engine(base.clone());
        // Reached back a year, all of it: only the pass is left.
        let now = now_ms();
        let synced = |reconcile: Option<graph::Reconcile>| {
            let state = graph::SyncState {
                delta_link: Some(format!("{base}/delta")),
                since: Some(now - 30 * 86_400_000),
                older_than: Some(now - 400 * 86_400_000),
                complete: true,
                reconcile,
                ..Default::default()
            };
            graph::save_state(&engine.store(), account_id, &state).unwrap();
        };
        synced(None);
        assert!(engine.backfill_due(account_id), "once after the start");
        let report = engine.backfill(account_id).await.unwrap();
        assert!(!report.changed());
        let asked = seen.lock().unwrap().clone();
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert!(
            asked[0].starts_with("/me/mailFolders/inbox/messages?$filter=receivedDateTime+lt+")
        );
        let unpaused = || engine.paces_lock().get_mut(&account_id).unwrap().next = None;
        unpaused();
        assert!(!engine.backfill_due(account_id), "the pass is over");

        // A pass under way goes on; a new one starts every hour.
        synced(Some(graph::Reconcile {
            before: now - 100 * 86_400_000,
            until: now - 400 * 86_400_000,
        }));
        assert!(engine.backfill_due(account_id));
        synced(None);
        assert!(!engine.backfill_due(account_id));
        engine.paces_lock().get_mut(&account_id).unwrap().reconciled =
            Some(Instant::now() - RECONCILE_EVERY);
        assert!(engine.backfill_due(account_id));

        // Not while Microsoft asks the account to wait.
        graph::tests::pause_until(
            &engine.standing(account_id),
            Some(tokio::time::Instant::now() + Duration::from_secs(5)),
        );
        assert!(!engine.backfill_due(account_id));
        graph::tests::pause_until(&engine.standing(account_id), None);
        assert!(engine.backfill_due(account_id));

        // Nothing older than the delta's window: no pass.
        let state = graph::SyncState {
            delta_link: Some(format!("{base}/delta")),
            since: Some(now - 30 * 86_400_000),
            complete: true,
            ..Default::default()
        };
        graph::save_state(&engine.store(), account_id, &state).unwrap();
        assert!(!engine.backfill_due(account_id));
    }

    #[tokio::test]
    async fn an_imap_search_brings_the_bodies_of_mail_stored_without_them() {
        // Found by what's only in its body, which isn't stored.
        let raw = b"From: ana@example.com\r\nSubject: Files\r\nMessage-ID: <z@example.com>\r\n\
                    Content-Type: text/plain\r\n\r\nThe zebra crossing plans.\r\n"
            .to_vec();
        let imap = ImapStandIn::start(7, vec![(42, raw.clone())]).await;
        let mut store = Store::open_in_memory().unwrap();
        let account = imap_account(&store, &imap.server, 7);
        store
            .set_sync_state(account.id, r#"{"uidValidity":7,"floor":40}"#)
            .unwrap();
        let id = store_head(&mut store, account.id, 42, &raw);
        let read = spark_store::Patch {
            unread: Some(false),
            ..Default::default()
        };
        store.update(id, &read, now_ms()).unwrap();
        assert!(store.search("zebra").unwrap().is_empty());
        let store = Arc::new(Mutex::new(store));
        let engine = Engine::new(store.clone(), Arc::new(TestSecrets));

        assert_eq!(engine.search_server(account.id, "zebra").await.unwrap(), 1);
        {
            let s = store.lock().unwrap();
            let content = s.content(id).unwrap().unwrap();
            assert!(!content.body_missing);
            assert_eq!(content.text.trim(), "The zebra crossing plans.");
            let m = s.message(id).unwrap().unwrap();
            assert!(!m.unread, "what the user changed stays");
            assert_eq!(s.search("zebra").unwrap()[0].id, id);
        }
        // Asked again, it has it.
        assert_eq!(engine.search_server(account.id, "zebra").await.unwrap(), 0);
    }

    /// `zebra` mail in full, number `n`, as `store_head` stores it.
    fn zebra(n: u32) -> Vec<u8> {
        format!(
            "From: ana@example.com\r\nSubject: Files\r\nMessage-ID: <z{n}@example.com>\r\n\
             Content-Type: text/plain\r\n\r\nThe zebra crossing, part {n}.\r\n"
        )
        .into_bytes()
    }

    /// An IMAP account searched in its Archive too, with no inbox hits.
    fn archived_search(imap: &ImapStandIn) -> (Arc<Mutex<Store>>, Account, i64) {
        let mut store = Store::open_in_memory().unwrap();
        let account = imap_account(&store, &imap.server, 7);
        let state = imap::SyncState {
            uid_validity: Some(7),
            floor: Some(40),
            archive: Some("Archive".into()),
            ..Default::default()
        };
        store.set_sync_state(account.id, &to_json(&state)).unwrap();
        store
            .replace_folders(
                account.id,
                &[RemoteFolder {
                    remote_id: "Archive".into(),
                    parent: None,
                    name: "Archive".into(),
                    role: Some(FolderRole::Archive),
                    selectable: true,
                }],
            )
            .unwrap();
        let folder = store.folders().unwrap()[0].id;
        imap.hold(imap::INBOX, 7, Vec::new());
        (Arc::new(Mutex::new(store)), account, folder)
    }

    #[tokio::test]
    async fn an_imap_search_brings_bodies_of_mail_archived_elsewhere() {
        let imap = ImapStandIn::start(7, Vec::new()).await;
        let (store, account, folder) = archived_search(&imap);
        // Stored from the inbox without its body; archived elsewhere since,
        // before the Archive's sync listed it there.
        let id = store_head(&mut store.lock().unwrap(), account.id, 42, &zebra(1));
        imap.hold("Archive", 9, vec![(7, zebra(1))]);
        let engine = Engine::new(store.clone(), Arc::new(TestSecrets));

        assert_eq!(engine.search_server(account.id, "zebra").await.unwrap(), 1);
        {
            let s = store.lock().unwrap();
            let content = s.content(id).unwrap().unwrap();
            assert!(!content.body_missing);
            assert_eq!(content.text.trim(), "The zebra crossing, part 1.");
            assert_eq!(s.search("crossing").unwrap()[0].id, id);
            assert_eq!(s.folder_uids(folder).unwrap()[&7], id, "listed there");
        }
        assert_eq!(engine.search_server(account.id, "zebra").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn an_imap_search_brings_the_rest_of_the_bodies_the_next_time() {
        let imap = ImapStandIn::start(7, Vec::new()).await;
        let (store, account, folder) = archived_search(&imap);
        // 101 in the Archive, as its sync listed them, stored without bodies.
        let mut members = Vec::new();
        let mut archived = Vec::new();
        for n in 1..=101 {
            let id = store_head(&mut store.lock().unwrap(), account.id, 1000 + n, &zebra(n));
            members.push((id, Some(n)));
            archived.push((n, zebra(n)));
        }
        store
            .lock()
            .unwrap()
            .set_folder_members(folder, &members)
            .unwrap();
        imap.hold("Archive", 9, archived);
        let engine = Engine::new(store.clone(), Arc::new(TestSecrets));

        assert_eq!(
            engine.search_server(account.id, "zebra").await.unwrap(),
            crate::search::SERVER_HITS
        );
        let bodyless = || {
            members
                .iter()
                .filter(|(id, _)| store.lock().unwrap().body_missing(*id).unwrap())
                .map(|(_, uid)| uid.unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(bodyless(), [1], "the newest first");
        assert_eq!(engine.search_server(account.id, "zebra").await.unwrap(), 1);
        assert!(bodyless().is_empty());
    }

    #[tokio::test]
    async fn the_background_download_waits_for_the_rest_and_after_failing() {
        // A server that isn't there.
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap().port()
        };
        let server = Server {
            host: "127.0.0.1".into(),
            port,
            security: imap::Security::Plain,
            trusted_cert: None,
        };
        let mut store = Store::open_in_memory().unwrap();
        let account = imap_account(&store, &server, 7);
        let id = store_imap(
            &mut store,
            account.id,
            "INBOX",
            42,
            b"Message-ID: <a@x>\r\n\r\n",
        );
        let store = Arc::new(Mutex::new(store));
        let engine = Engine::new(store.clone(), Arc::new(TestSecrets));
        assert!(!engine.backfill_due(account.id), "before the first sync");
        let synced = |window: Option<u32>, complete: bool| {
            let state = imap::SyncState {
                uid_validity: Some(7),
                floor: Some(500),
                window,
                complete,
                ..Default::default()
            };
            store
                .lock()
                .unwrap()
                .set_sync_state(account.id, &to_json(&state))
                .unwrap();
        };
        synced(None, false);
        assert!(engine.backfill_due(account.id));
        synced(Some(500), false);
        assert!(!engine.backfill_due(account.id), "down to the window");
        synced(Some(100), true);
        assert!(!engine.backfill_due(account.id), "nothing older");
        synced(Some(100), false);
        assert!(engine.backfill_due(account.id));

        // A change to push, or mail to send, goes first.
        let done = spark_store::Patch {
            state: Some(spark_store::MessageState::Done),
            ..Default::default()
        };
        store
            .lock()
            .unwrap()
            .update(id, &done, now_ms() - 60_000)
            .unwrap();
        assert!(!engine.backfill_due(account.id));
        store.lock().unwrap().drop_pending(id).unwrap();
        assert!(engine.backfill_due(account.id));
        let queued = store
            .lock()
            .unwrap()
            .queue_send(account.id, "{}", now_ms() - 1, &[])
            .unwrap();
        assert!(!engine.backfill_due(account.id));
        assert!(store.lock().unwrap().cancel_send(queued).unwrap());
        assert!(engine.backfill_due(account.id));

        // Even a change still in its undo window, or a draft just saved.
        let pin = spark_store::Patch {
            pinned: Some(true),
            ..Default::default()
        };
        store.lock().unwrap().update(id, &pin, now_ms()).unwrap();
        assert!(!engine.backfill_due(account.id));
        // A step that comes after, once it has the account's turn, does
        // nothing: it doesn't go to the server, which isn't there.
        assert_eq!(
            engine.backfill(account.id).await.unwrap(),
            Report::default()
        );
        assert!(engine.paces_lock().get(&account.id).is_none());
        store.lock().unwrap().drop_pending(id).unwrap();
        let draft = spark_store::DraftSave {
            account_id: account.id,
            draft: "{}",
            send_later: None,
            from_name: "Me",
            from_address: "me@example.com",
            to: &[],
            cc: &[],
            bcc: &[],
            subject: "Plans",
            snippet: "",
            text: "",
            html: None,
            in_reply_to: None,
            references: None,
            attachments: &[],
            files: &[],
        };
        let saved = store
            .lock()
            .unwrap()
            .save_draft(None, &draft, now_ms())
            .unwrap();
        assert!(!engine.backfill_due(account.id));
        store.lock().unwrap().drop_draft_upload(saved).unwrap();
        assert!(engine.backfill_due(account.id));

        // A step that fails waits before the next.
        assert!(engine.backfill(account.id).await.is_err());
        assert!(!engine.backfill_due(account.id));
        let pace = engine.paces_lock().remove(&account.id).unwrap();
        assert_eq!(pace.failures, 1);
        let wait = pace.next.unwrap() - Instant::now();
        assert!(wait > BACKFILL_RETRY - Duration::from_secs(5) && wait <= BACKFILL_RETRY);
        assert!(engine.backfill_due(account.id), "once the wait is over");
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

    /// An engine with a Microsoft account signed in to the Graph stand-in
    /// at `base`.
    fn microsoft_engine(base: String) -> (Engine, i64) {
        microsoft_engine_in(Store::open_in_memory().unwrap(), base)
    }

    /// `microsoft_engine`, keeping its mail in `store`.
    fn microsoft_engine_in(store: Store, base: String) -> (Engine, i64) {
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
        (signed_in(store, base, account.id), account.id)
    }

    /// An engine for `store`, its Microsoft account `account_id` signed in
    /// to the Graph stand-in at `base`.
    fn signed_in(store: Store, base: String, account_id: i64) -> Engine {
        let mut engine = Engine::new(Arc::new(Mutex::new(store)), Arc::new(TestSecrets));
        engine.graph_base = base;
        engine.cache_token(
            account_id,
            &oauth::Tokens {
                access_token: "token".into(),
                refresh_token: None,
                expires_at: now_ms() + 3_600_000,
            },
        );
        engine
    }

    /// Saves an auto-draft replying to a new email in `account`, uploaded
    /// as `copy` (unless it's `None`) with `version` recorded for it.
    fn own_draft(
        store: &mut Store,
        account: i64,
        copy: Option<spark_store::DraftCopy>,
        version: Option<&str>,
    ) -> i64 {
        let folders = [RemoteFolder {
            remote_id: "Drafts".into(),
            parent: None,
            name: "Drafts".into(),
            role: Some(FolderRole::Drafts),
            selectable: true,
        }];
        if store.folders().unwrap().is_empty() {
            store.replace_folders(account, &folders).unwrap();
        }
        let Added::New(original) = store
            .add_incoming(
                &Incoming {
                    account_id: account,
                    from_address: "ana@example.com".into(),
                    subject: "Lunch".into(),
                    ..Default::default()
                },
                false,
            )
            .unwrap()
        else {
            panic!("new")
        };
        let id = store
            .save_draft(
                None,
                &spark_store::DraftSave {
                    account_id: account,
                    draft: "{}",
                    send_later: None,
                    from_name: "Me",
                    from_address: "me@example.com",
                    to: &["ana@example.com".to_owned()],
                    cc: &[],
                    bcc: &[],
                    subject: "Re: Lunch",
                    snippet: "Sure.",
                    text: "Sure.",
                    html: None,
                    in_reply_to: None,
                    references: None,
                    attachments: &[],
                    files: &[],
                },
                1,
            )
            .unwrap();
        let option = [spark_store::DraftOption {
            title: String::new(),
            body: "Sure.".into(),
        }];
        store.add_auto_draft(id, original, &option, 0, 1).unwrap();
        if let Some(copy) = copy {
            let upload = store
                .draft_uploads(account, i64::MAX)
                .unwrap()
                .into_iter()
                .find(|u| u.message_id == id)
                .unwrap();
            store.draft_uploaded(&upload, &copy, &[]).unwrap();
            store.auto_draft_uploaded(id, version).unwrap();
        }
        id
    }

    #[tokio::test]
    async fn only_a_microsoft_draft_still_as_this_app_put_it_is_deleted() {
        let (base, seen) = crate::graph::tests::serve(|asked, _| match asked.path.as_str() {
            "/me/messages/edited?$select=changeKey" => {
                Answer::json(serde_json::json!({ "changeKey": "v2" }))
            }
            "/me/messages/untouched/permanentDelete" => Answer::status(204),
            "/me/messages/untouched?$select=changeKey"
            | "/me/messages/unrecorded?$select=changeKey" => {
                Answer::json(serde_json::json!({ "changeKey": "v1" }))
            }
            _ => Answer::status(404),
        })
        .await;
        let (engine, account) = microsoft_engine(base);
        let copy = |remote: &str| spark_store::DraftCopy {
            remote_id: Some(remote.into()),
            ..Default::default()
        };
        let ids: Vec<i64> = {
            let mut store = engine.store();
            vec![
                own_draft(&mut store, account, Some(copy("untouched")), Some("v1")),
                own_draft(&mut store, account, Some(copy("edited")), Some("v1")),
                own_draft(&mut store, account, Some(copy("unrecorded")), None),
                own_draft(&mut store, account, Some(copy("gone")), Some("v1")),
                own_draft(&mut store, account, None, None),
            ]
        };
        // `may` saying no keeps even an untouched one.
        assert_eq!(
            engine.delete_own_draft(ids[0], |_| false).await.unwrap(),
            OwnDraft::Kept
        );
        assert!(engine.store().message(ids[0]).unwrap().is_some());
        let deletes = |seen: &crate::graph::tests::Seen| -> Vec<String> {
            seen.take()
                .into_iter()
                .filter(|a| a.path.ends_with("/permanentDelete"))
                .map(|a| a.path)
                .collect()
        };
        assert!(deletes(&seen).is_empty());
        // Its copy goes now, right after its changeKey was checked: queued
        // for a later round, an edit made in Outlook meanwhile would go too.
        assert_eq!(
            engine.delete_own_draft(ids[0], |_| true).await.unwrap(),
            OwnDraft::Deleted
        );
        assert_eq!(deletes(&seen), ["/me/messages/untouched/permanentDelete"]);
        assert!(engine.store().message(ids[0]).unwrap().is_none());
        assert!(engine.store().draft_removals(account).unwrap().is_empty());
        // Outlook changed it since it went up, or its version wasn't kept.
        for id in [ids[1], ids[2]] {
            assert_eq!(
                engine.delete_own_draft(id, |_| true).await.unwrap(),
                OwnDraft::Changed
            );
            assert!(engine.store().message(id).unwrap().is_some());
        }
        assert!(deletes(&seen).is_empty());
        assert!(matches!(
            engine.delete_own_draft(ids[3], |_| true).await,
            Err(Error::Gone)
        ));
        // Not on the server yet: only here.
        assert_eq!(
            engine.delete_own_draft(ids[4], |_| true).await.unwrap(),
            OwnDraft::Deleted
        );
        assert!(engine.store().message(ids[4]).unwrap().is_none());
    }

    #[tokio::test]
    async fn cleanup_waiting_on_a_round_sees_the_copy_it_uploaded() {
        let (base, seen) = crate::graph::tests::serve(|asked, _| match asked.path.as_str() {
            // Outlook changed it as soon as the round put it there.
            "/me/messages/late?$select=changeKey" => {
                Answer::json(serde_json::json!({ "changeKey": "v2" }))
            }
            _ => Answer::status(404),
        })
        .await;
        let (engine, account) = microsoft_engine(base);
        let id = own_draft(&mut engine.store(), account, None, None);
        // A round has the account's turn, uploading it.
        let turn = engine.turn(account);
        let round = turn.lock().await;
        let cleanup = engine.delete_own_draft(id, |_| true);
        tokio::pin!(cleanup);
        let waited = std::time::Duration::from_millis(50);
        assert!(tokio::time::timeout(waited, &mut cleanup).await.is_err());
        {
            let mut store = engine.store();
            let upload = store
                .draft_uploads(account, i64::MAX)
                .unwrap()
                .into_iter()
                .find(|u| u.message_id == id)
                .unwrap();
            let copy = spark_store::DraftCopy {
                remote_id: Some("late".into()),
                ..Default::default()
            };
            store.draft_uploaded(&upload, &copy, &[]).unwrap();
            store.auto_draft_uploaded(id, Some("v1")).unwrap();
        }
        drop(round);
        // Not taken for one only here, which would queue its copy's deletion
        // with no check.
        assert_eq!(cleanup.await.unwrap(), OwnDraft::Changed);
        assert!(engine.store().message(id).unwrap().is_some());
        assert!(engine.store().draft_removals(account).unwrap().is_empty());
        let asked: Vec<_> = seen.take().into_iter().map(|a| a.path).collect();
        assert_eq!(asked, ["/me/messages/late?$select=changeKey"]);
    }

    /// A scripted IMAP server: `reply` gives the untagged lines for each
    /// command (without its tag); every command then gets OK. Returns where
    /// it is, and the commands it saw once the session ends.
    async fn scripted_imap(
        reply: impl Fn(&str) -> String + Send + 'static,
    ) -> (Server, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = Server {
            host: "127.0.0.1".into(),
            port: listener.local_addr().unwrap().port(),
            security: imap::Security::Plain,
            trusted_cert: None,
        };
        let task = tokio::spawn(async move {
            let mut commands = Vec::new();
            let Ok((stream, _)) = listener.accept().await else {
                return commands;
            };
            let mut io = BufReader::new(stream);
            let _ = io.get_mut().write_all(b"* OK scripted\r\n").await;
            loop {
                let mut line = String::new();
                if io.read_line(&mut line).await.unwrap_or(0) == 0 {
                    break;
                }
                let (tag, command) = line.trim_end().split_once(' ').unwrap_or(("*", ""));
                commands.push(command.to_owned());
                let untagged = match command {
                    "CAPABILITY" => "* CAPABILITY IMAP4rev1 UIDPLUS\r\n".to_owned(),
                    "LOGOUT" => "* BYE\r\n".to_owned(),
                    _ => reply(command),
                };
                let answer = format!("{untagged}{tag} OK done\r\n");
                if io.get_mut().write_all(answer.as_bytes()).await.is_err() || command == "LOGOUT" {
                    break;
                }
            }
            commands
        });
        (server, task)
    }

    /// Drafts holds `<own@example.com>` at `uid`.
    fn drafts_holding(uid: u32) -> impl Fn(&str) -> String + Send + 'static {
        move |command| {
            if command.starts_with("SELECT ") {
                "* 1 EXISTS\r\n* OK [UIDVALIDITY 1] valid\r\n".into()
            } else if command.starts_with("UID SEARCH ") {
                format!("* SEARCH {uid}\r\n")
            } else if command.starts_with("UID FETCH ") {
                let header = "Message-ID: <own@example.com>\r\n\r\n";
                format!(
                    "* 1 FETCH (UID {uid} BODY[HEADER.FIELDS (MESSAGE-ID)] {{{}}}\r\n{header})\r\n",
                    header.len()
                )
            } else {
                String::new()
            }
        }
    }

    #[tokio::test]
    async fn only_an_imap_draft_still_as_this_app_put_it_is_deleted() {
        let copy = |uid: Option<u32>| spark_store::DraftCopy {
            mailbox: Some("Drafts".into()),
            uid,
            message_id: Some("own@example.com".into()),
            ..Default::default()
        };
        // Drafts is at UIDVALIDITY 1 on the server. Another app replaced UID
        // 5 with an edited UID 20, keeping the Message-ID, before a sync saw
        // it; or Drafts was made again since UID 5 went up at UIDVALIDITY 2,
        // and its UID 5 now is some other copy: it stays, there and here.
        for (validity, server_has, expected) in [
            (1, 5, OwnDraft::Deleted),
            (1, 20, OwnDraft::Changed),
            (2, 5, OwnDraft::Changed),
        ] {
            let version = drafts::imap_version("Drafts", validity, 5);
            let (server, commands) = scripted_imap(drafts_holding(server_has)).await;
            let mut store = Store::open_in_memory().unwrap();
            let account = imap_account(&store, &server, 1);
            let id = own_draft(&mut store, account.id, Some(copy(Some(5))), Some(&version));
            let engine = Engine::new(Arc::new(Mutex::new(store)), Arc::new(TestSecrets));
            assert_eq!(
                engine.delete_own_draft(id, |_| true).await.unwrap(),
                expected
            );
            let commands = commands.await.unwrap();
            let expunged: Vec<_> = commands
                .iter()
                .filter(|c| c.starts_with("UID EXPUNGE"))
                .collect();
            if expected == OwnDraft::Deleted {
                assert_eq!(expunged, ["UID EXPUNGE 5"]);
                assert!(engine.store().message(id).unwrap().is_none());
                assert!(
                    engine
                        .store()
                        .draft_removals(account.id)
                        .unwrap()
                        .is_empty()
                );
            } else {
                assert!(expunged.is_empty(), "{commands:?}");
                assert!(engine.store().message(id).unwrap().is_some());
            }
        }
        // Uploaded, but its UID was never learned: kept without asking.
        let mut store = Store::open_in_memory().unwrap();
        let account = imap_account(&store, &Server::new("127.0.0.1", 9), 1);
        let id = own_draft(&mut store, account.id, Some(copy(None)), None);
        let engine = Engine::new(Arc::new(Mutex::new(store)), Arc::new(TestSecrets));
        assert_eq!(
            engine.delete_own_draft(id, |_| true).await.unwrap(),
            OwnDraft::Changed
        );
    }

    /// A Microsoft account signed in to the Graph stand-in at `base`, with
    /// message m1: four files, the last stored without its attachment id.
    fn graph_engine(base: String, dir: &Path) -> (Engine, i64) {
        let (engine, account_id) = microsoft_engine(base);
        let engine = engine.with_attachment_cache(dir.to_owned());
        let attached = |name: &str, id: Option<&str>| Attachment {
            name: name.into(),
            mime: "application/pdf".into(),
            size: 5,
            remote_id: id.map(str::to_owned),
        };
        let incoming = Incoming {
            account_id,
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
        let (Added::New(id) | Added::Known(id)) =
            engine.store().add_incoming(&incoming, false).unwrap();
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

    /// Where a synced account's inbox delta query stands, before `/delta1`.
    const AT_DELTA1: &str = r#"{"deltaLink":"/delta1","replyToSelected":true}"#;

    /// The account's inbox delta link.
    fn delta_link(engine: &Engine, account_id: i64) -> Option<String> {
        let account = engine.account(account_id).unwrap();
        from_json::<graph::SyncState>(&account.sync_state).delta_link
    }

    /// A Graph stand-in for a synced account's inbox: `/delta1` has four
    /// new messages, n1 to n4, and `/delta2` a fifth, n5, each with a file
    /// named for it (`n1.pdf`); `/delta3` has nothing new. `failing` gives
    /// Graph's answer to listing message `id`'s attachments when it fails.
    fn inbox(
        failing: impl Fn(&str) -> Option<Answer> + Send + Sync + 'static,
    ) -> impl Fn(&Asked, usize) -> Answer + Send + Sync + 'static {
        use serde_json::json;
        move |asked, _| {
            let route = asked.path.split('?').next().unwrap();
            let new = |numbers: &[usize], next: &str| {
                let value: Vec<_> = numbers
                    .iter()
                    .map(|n| {
                        json!({
                            "id": format!("n{n}"), "subject": format!("Message n{n}"),
                            "isRead": false, "receivedDateTime": "2026-10-27T09:00:00Z",
                            "hasAttachments": true,
                            "from": { "emailAddress": { "name": "Ana", "address": "ana@contoso.com" } },
                            "body": { "contentType": "text", "content": "Hi" },
                        })
                    })
                    .collect();
                Answer::json(json!({ "value": value, "@odata.deltaLink": next }))
            };
            match route {
                "/delta1" => return new(&[1, 2, 3, 4], "/delta2"),
                "/delta2" => return new(&[5], "/delta3"),
                "/delta3" => return new(&[], "/delta3"),
                _ => {}
            }
            let listed = route
                .strip_prefix("/me/messages/")
                .and_then(|rest| rest.strip_suffix("/attachments"));
            match listed {
                Some(id) => failing(id).unwrap_or_else(|| {
                    Answer::json(json!({ "value": [
                        { "id": "a1", "name": format!("{id}.pdf"), "contentType": "application/pdf", "size": 10 }
                    ] }))
                }),
                None => Answer::status(404),
            }
        }
    }

    /// The account's list of mail to fetch what's missing of.
    fn repair_list(engine: &Engine, account_id: i64) -> Vec<graph::Repair> {
        let account = engine.account(account_id).unwrap();
        from_json::<graph::SyncState>(&account.sync_state).repair
    }

    /// The stored messages' subjects with the names of their files.
    fn files(engine: &Engine) -> Vec<(String, Vec<String>)> {
        let store = engine.store();
        let mut files: Vec<_> = store
            .messages()
            .unwrap()
            .into_iter()
            .map(|m| {
                let content = store.content(m.id).unwrap().unwrap();
                let names = content.attachments.into_iter().map(|a| a.name);
                (m.subject, names.collect())
            })
            .collect();
        files.sort();
        files
    }

    /// `files` for the `inbox` stand-in's messages `numbers`, each with its
    /// file but those `missing` theirs.
    fn whole(
        numbers: std::ops::RangeInclusive<usize>,
        missing: &[usize],
    ) -> Vec<(String, Vec<String>)> {
        numbers
            .map(|n| {
                let files = match missing.contains(&n) {
                    true => Vec::new(),
                    false => vec![format!("n{n}.pdf")],
                };
                (format!("Message n{n}"), files)
            })
            .collect()
    }

    #[tokio::test]
    async fn a_message_graph_keeps_failing_never_holds_up_the_inbox() {
        let n2 = |id: &str| (id == "n2").then(|| Answer::status(500));
        let (base, seen) = graph::tests::serve(inbox(n2)).await;
        let (engine, account_id) = microsoft_engine(base);
        engine
            .store()
            .set_sync_state(account_id, AT_DELTA1)
            .unwrap();
        let n2_listings = |asked: Vec<Asked>| {
            let n2 = asked
                .iter()
                .filter(|a| a.path.starts_with("/me/messages/n2/"));
            n2.count()
        };

        // The first round stores n2 without its file and the others with
        // theirs, and moves on. n2 is listed, and tried again at its end.
        let report = engine.run(account_id, true).await.unwrap();
        assert_eq!((report.new, report.repaired), (4, 0));
        assert_eq!(n2_listings(seen.take()), 2);
        assert_eq!(delta_link(&engine, account_id).as_deref(), Some("/delta2"));
        assert_eq!(files(&engine), whole(1..=4, &[2]));
        let listed = repair_list(&engine, account_id);
        assert_eq!(
            (listed.len(), listed[0].id.as_str(), listed[0].tries),
            (1, "n2", 1)
        );

        // New mail keeps coming, and every round tries n2 once more.
        let report = engine.run(account_id, true).await.unwrap();
        assert_eq!(report.new, 1);
        assert_eq!(delta_link(&engine, account_id).as_deref(), Some("/delta3"));
        assert_eq!(files(&engine), whole(1..=5, &[2]));
        for tries in 2..=graph::REPAIR_TRIES + 2 {
            if tries > 2 {
                engine.run(account_id, true).await.unwrap();
            }
            assert_eq!(n2_listings(seen.take()), 1, "try {tries}");
            assert_eq!(repair_list(&engine, account_id)[0].tries, tries);
        }

        // Once it's been failing for a week too, it's given up on.
        let mut state: graph::SyncState =
            from_json(&engine.account(account_id).unwrap().sync_state);
        state.repair[0].since -= graph::REPAIR_FOR_MS;
        engine
            .store()
            .set_sync_state(account_id, &to_json(&state))
            .unwrap();
        engine.run(account_id, true).await.unwrap();
        assert_eq!(n2_listings(seen.take()), 1);
        assert!(repair_list(&engine, account_id).is_empty());
        engine.run(account_id, true).await.unwrap();
        assert_eq!(n2_listings(seen.take()), 0);
        assert_eq!(files(&engine), whole(1..=5, &[2]));
        assert_eq!(delta_link(&engine, account_id).as_deref(), Some("/delta3"));
    }

    #[tokio::test]
    async fn an_outage_over_three_rounds_loses_no_files() {
        // Graph answers every attachment listing 503 while it's down,
        // asking to be tried again at once.
        let down = Arc::new(AtomicBool::new(true));
        let unavailable = down.clone();
        let failing = move |_: &str| {
            let down = unavailable.load(SeqCst);
            down.then(|| Answer::status(503).with_retry_after(0))
        };
        let (base, _) = graph::tests::serve(inbox(failing)).await;
        let (engine, account_id) = microsoft_engine(base);
        engine
            .store()
            .set_sync_state(account_id, AT_DELTA1)
            .unwrap();

        // Three rounds store the mail without them, and list it.
        for (round, new) in [(1, 4), (2, 1), (3, 0)] {
            let report = engine.run(account_id, true).await.unwrap();
            assert_eq!((report.new, report.repaired), (new, 0), "round {round}");
        }
        assert_eq!(delta_link(&engine, account_id).as_deref(), Some("/delta3"));
        assert_eq!(files(&engine), whole(1..=5, &[1, 2, 3, 4, 5]));
        let listed = repair_list(&engine, account_id);
        let mut ids: Vec<&str> = listed.iter().map(|r| r.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, ["n1", "n2", "n3", "n4", "n5"]);
        assert!(
            listed.iter().all(|r| r.tries == 0),
            "Graph being unavailable isn't the messages' doing"
        );

        // The first round once it's back fetches them all.
        down.store(false, SeqCst);
        let report = engine.run(account_id, true).await.unwrap();
        assert_eq!((report.new, report.repaired), (0, 5));
        assert!(report.changed());
        assert_eq!(files(&engine), whole(1..=5, &[]));
        assert!(repair_list(&engine, account_id).is_empty());
    }

    #[tokio::test]
    async fn inbox_checks_leave_repairs_to_the_sweep() {
        let down = Arc::new(AtomicBool::new(true));
        let unavailable = down.clone();
        let failing = move |_: &str| {
            let down = unavailable.load(SeqCst);
            down.then(|| Answer::status(503).with_retry_after(0))
        };
        let (base, seen) = graph::tests::serve(inbox(failing)).await;
        let (engine, account_id) = microsoft_engine(base);
        engine
            .store()
            .set_sync_state(account_id, AT_DELTA1)
            .unwrap();
        engine.run(account_id, true).await.unwrap();
        assert_eq!(repair_list(&engine, account_id).len(), 4);
        down.store(false, SeqCst);
        seen.take();

        // An inbox check stores new mail whole, and leaves the list be.
        let check = Round {
            fetch: true,
            ..Default::default()
        };
        let report = engine.run_round(account_id, &check).await.unwrap();
        assert_eq!((report.new, report.repaired), (1, 0));
        let listings: Vec<String> = seen
            .take()
            .into_iter()
            .map(|a| a.path.split('?').next().unwrap().to_owned())
            .filter(|path| path.ends_with("/attachments"))
            .collect();
        assert_eq!(listings, ["/me/messages/n5/attachments"]);
        assert_eq!(files(&engine), whole(1..=5, &[1, 2, 3, 4]));
        assert_eq!(repair_list(&engine, account_id).len(), 4);

        // The sweep fetches what's missing.
        let sweep = Round {
            fetch: true,
            repair: true,
            ..Default::default()
        };
        let report = engine.run_round(account_id, &sweep).await.unwrap();
        assert_eq!((report.new, report.repaired), (0, 4));
        assert_eq!(files(&engine), whole(1..=5, &[]));
        assert!(repair_list(&engine, account_id).is_empty());
    }

    #[tokio::test]
    async fn mail_listed_before_a_restart_gets_its_files_after() {
        let path = std::env::temp_dir().join(format!(
            "spark-graph-repair-{}.sqlite",
            drafts::new_message_id("test")
        ));
        let n2 = |id: &str| (id == "n2").then(|| Answer::status(500));
        let (base, _) = graph::tests::serve(inbox(n2)).await;
        let account_id = {
            let (engine, account_id) = microsoft_engine_in(Store::open(&path).unwrap(), base);
            engine
                .store()
                .set_sync_state(account_id, AT_DELTA1)
                .unwrap();
            engine.run(account_id, true).await.unwrap();
            assert_eq!(repair_list(&engine, account_id).len(), 1);
            account_id
        };

        // Opened again, with Graph giving n2's file now.
        let (base, _) = graph::tests::serve(inbox(|_| None)).await;
        let engine = signed_in(Store::open(&path).unwrap(), base, account_id);
        let listed = repair_list(&engine, account_id);
        assert_eq!((listed.len(), listed[0].id.as_str()), (1, "n2"));
        let report = engine.run(account_id, true).await.unwrap();
        assert_eq!((report.new, report.repaired), (1, 1));
        assert_eq!(files(&engine), whole(1..=5, &[]));
        assert!(repair_list(&engine, account_id).is_empty());
        drop(engine);
        for suffix in ["", "-wal", "-shm"] {
            let mut file = path.clone().into_os_string();
            file.push(suffix);
            let _ = std::fs::remove_file(file);
        }
    }

    #[tokio::test]
    async fn mail_killed_as_it_was_listed_for_repair_comes_whole_after_a_restart() {
        let path = std::env::temp_dir().join(format!(
            "spark-graph-killed-{}.sqlite",
            drafts::new_message_id("test")
        ));
        // Graph fails to list n2's file while `down`.
        let down = Arc::new(AtomicBool::new(true));
        let failing = down.clone();
        let n2 = move |id: &str| (id == "n2" && failing.load(SeqCst)).then(|| Answer::status(500));
        let (base, _) = graph::tests::serve(inbox(n2)).await;
        let account_id = {
            let (engine, account_id) =
                microsoft_engine_in(Store::open(&path).unwrap(), base.clone());
            engine
                .store()
                .set_sync_state(account_id, AT_DELTA1)
                .unwrap();
            // The app is killed as n2 is listed: none of its page is
            // stored, and the delta query hasn't moved.
            graph::tests::LISTING_FAILS.set(true);
            assert!(engine.run(account_id, true).await.is_err());
            assert!(files(&engine).is_empty());
            assert!(repair_list(&engine, account_id).is_empty());
            assert_eq!(delta_link(&engine, account_id).as_deref(), Some("/delta1"));
            account_id
        };

        // Opened again, the page comes again: stored, with n2 listed.
        let engine = signed_in(Store::open(&path).unwrap(), base, account_id);
        let report = engine.run(account_id, true).await.unwrap();
        assert_eq!((report.new, report.repaired), (4, 0));
        assert_eq!(files(&engine), whole(1..=4, &[2]));
        let listed = repair_list(&engine, account_id);
        assert_eq!((listed.len(), listed[0].id.as_str()), (1, "n2"));

        // And once Graph gives it, n2's file comes.
        down.store(false, SeqCst);
        let report = engine.run(account_id, true).await.unwrap();
        assert_eq!((report.new, report.repaired), (1, 1));
        assert_eq!(files(&engine), whole(1..=5, &[]));
        assert!(repair_list(&engine, account_id).is_empty());
        drop(engine);
        for suffix in ["", "-wal", "-shm"] {
            let mut file = path.clone().into_os_string();
            file.push(suffix);
            let _ = std::fs::remove_file(file);
        }
    }

    #[tokio::test]
    async fn repaired_mail_reaches_the_message_list() {
        let down = Arc::new(AtomicBool::new(true));
        let failing = down.clone();
        let n2 = move |id: &str| (id == "n2" && failing.load(SeqCst)).then(|| Answer::status(500));
        let (base, _) = graph::tests::serve(inbox(n2)).await;
        let (engine, account_id) = microsoft_engine(base);
        engine
            .store()
            .set_sync_state(account_id, AT_DELTA1)
            .unwrap();
        engine.run(account_id, true).await.unwrap();
        let (_, mark) = engine.store().messages_marked().unwrap();

        down.store(false, SeqCst);
        let report = engine.run(account_id, true).await.unwrap();
        assert_eq!(report.repaired, 1);
        assert!(report.changed(), "the app reads the list's changes");
        let changes = engine.store().message_changes(&mark.unwrap()).unwrap();
        let changed = changes.unwrap().messages;
        let n2 = changed.iter().find(|m| m.subject == "Message n2").unwrap();
        assert_eq!(n2.attachment.as_deref(), Some("n2.pdf"));
    }

    /// A Graph stand-in whose inbox delta (`/delta1`) has nothing new.
    /// Graph throttles it the first time, asking for a minute's pause.
    fn throttled_for_a_minute(asked: &Asked, attempt: usize) -> Answer {
        match (asked.path.as_str(), attempt) {
            ("/delta1", 0) => Answer::status(429).with_retry_after(60),
            ("/delta1", _) => Answer::json(serde_json::json!({
                "value": [], "@odata.deltaLink": "/delta2",
            })),
            _ => Answer::status(404),
        }
    }

    #[tokio::test]
    async fn a_throttled_account_sends_nothing_until_the_pause_ends() {
        let (base, seen) = graph::tests::serve(throttled_for_a_minute).await;
        let (engine, account_id) = microsoft_engine(base);
        engine
            .store()
            .set_sync_state(account_id, AT_DELTA1)
            .unwrap();

        // Graph asks for a minute's pause: the round fails as busy.
        let throttled = engine.run(account_id, true).await;
        assert!(
            matches!(throttled, Err(Error::Network { .. })),
            "{throttled:?}"
        );
        assert_eq!(seen.take().len(), 1);

        // The next round, with more than half a minute of it left, fails
        // as busy at once and sends nothing.
        let started = std::time::Instant::now();
        let held = engine.run(account_id, true).await;
        assert!(matches!(held, Err(Error::Network { .. })), "{held:?}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert!(seen.take().is_empty());

        // Near its end, a round waits for it, then syncs.
        let end = tokio::time::Instant::now() + std::time::Duration::from_millis(300);
        graph::tests::pause_until(&engine.standing(account_id), Some(end));
        engine.run(account_id, true).await.unwrap();
        let asked = seen.take();
        assert_eq!(asked.len(), 1, "the delta query");
        assert!(asked[0].at >= end.into_std());
        assert_eq!(delta_link(&engine, account_id).as_deref(), Some("/delta2"));
    }

    #[tokio::test]
    async fn a_search_keeps_to_the_accounts_pause_too() {
        let (base, seen) = graph::tests::serve(throttled_for_a_minute).await;
        let (engine, account_id) = microsoft_engine(base);
        engine
            .store()
            .set_sync_state(account_id, AT_DELTA1)
            .unwrap();
        let throttled = engine.run(account_id, true).await;
        assert!(
            matches!(throttled, Err(Error::Network { .. })),
            "{throttled:?}"
        );
        seen.take();

        let held = engine.search_server(account_id, "invoice").await;
        assert!(matches!(held, Err(Error::Network { .. })), "{held:?}");
        assert!(seen.take().is_empty());
    }

    /// A Tokyo series' occurrence, at 09:00 there (00:00 UTC) on October 15,
    /// all-day or not as `all_day` says, as Graph tells it in the zone asked.
    async fn tokyo_occurrence(all_day: bool) -> String {
        let (base, _) = crate::graph::tests::serve(move |asked: &Asked, _| {
            if asked.path != "/me/events/occ1?$select=start,originalStart,isAllDay" {
                return Answer::status(404);
            }
            let utc = asked.prefer.iter().any(|p| p.contains("\"UTC\""));
            let start = match (all_day, utc) {
                (true, _) => "2026-10-15T00:00:00.0000000",
                (false, true) => "2026-10-15T00:00:00.0000000",
                (false, false) => "2026-10-15T09:00:00.0000000",
            };
            Answer::json(serde_json::json!({
                "start": { "dateTime": start },
                "isAllDay": all_day,
            }))
        })
        .await;
        base
    }

    #[tokio::test]
    async fn a_series_clock_goes_by_whether_outlook_has_it_all_day_now() {
        fn pacific(ms: i64) -> String {
            calendar::utc_wall(ms - 7 * 3_600_000)
        }
        let zone = Zone {
            name: "Pacific Standard Time",
            wall: pacific,
            shift: 0,
        };
        let master = serde_json::json!({
            "recurrence": { "range": { "recurrenceTimeZone": "Tokyo Standard Time" } }
        });
        // Stored as all-day, since made 09:00 to 10:00 in Outlook: Tokyo is
        // 16 hours ahead.
        let graph = Graph::with_base(&tokyo_occurrence(false).await, "token");
        let start = graph::parse_iso_date("2026-10-15T00:00:00Z").unwrap();
        let clock = series_clock(&graph, "occ1", start, &master, zone)
            .await
            .unwrap();
        assert_eq!(
            (clock.shift, clock.current.as_str()),
            (16 * 60, "2026-10-15")
        );
        // Made all-day in Outlook: its date is the same on every clock.
        let graph = Graph::with_base(&tokyo_occurrence(true).await, "token");
        let clock = series_clock(&graph, "occ1", start, &master, zone)
            .await
            .unwrap();
        assert_eq!((clock.shift, clock.current.as_str()), (0, "2026-10-15"));
    }
}
