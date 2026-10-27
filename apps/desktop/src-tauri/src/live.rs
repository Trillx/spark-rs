//! Real accounts: the mail engine and one background worker per account.
//!
//! A worker loops: send what's due, push local changes once they're past the
//! undo window, and fetch new mail on Spark's schedule. IMAP inboxes are
//! watched with IDLE, so new mail comes in as it arrives; other inboxes are
//! checked every 30 seconds, and when the window comes to the front. Every
//! five minutes a sweep also syncs the folders the UI watches, lists the
//! folders again and fetches what Microsoft mail was stored without. Older
//! mail is reached back for when the UI asks.
//! Searches go to every account's server at once, beside the workers. It
//! tells the UI through events (`mailbox-changed`, `folders-changed`,
//! `accounts-changed`, `sync-status`, `send-status`), and the user through
//! notifications for new mail.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::join_all;
use serde::Serialize;
use spark_mail::engine::{folder_has_older, inbox_has_older};
use spark_mail::{Engine, Error, Round, Untrusted, Watch};
use spark_store::{Store, now_ms};
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter};
use tokio::sync::{Notify, oneshot};

use crate::mailbox::{SyncStatus, SyncView, certificate_of};
use crate::notify;
use crate::secrets::MailSecrets;

const TICK: Duration = Duration::from_secs(2);
/// How often an inbox IDLE doesn't watch is checked, as in Spark.
const INBOX_EVERY: Duration = Duration::from_secs(30);
/// How often the sweep runs, as in Spark: the inbox (watched or not), the
/// folders the UI watches, and the folder list.
const SWEEP_EVERY: Duration = Duration::from_secs(5 * 60);
const RETRY_AFTER: Duration = Duration::from_secs(30);
const SNOOZE_CHECK: Duration = Duration::from_secs(15);
/// A wait this much longer than `TICK` means the computer slept.
const SLEPT: Duration = Duration::from_secs(60);
/// How long after IDLE fails it's tried again, doubling up to
/// `IDLE_RETRY_MAX`. A server out of connections waits the most.
const IDLE_RETRY: Duration = Duration::from_secs(30);
const IDLE_RETRY_MAX: Duration = Duration::from_secs(5 * 60);
/// How long a search waits for a server, as in Spark.
const SEARCH_WAIT: Duration = Duration::from_secs(60);

pub struct Live {
    pub engine: Arc<Engine>,
    workers: Mutex<HashMap<i64, Worker>>,
    status: Arc<Mutex<HashMap<i64, SyncView>>>,
    /// Cancels the Microsoft sign-in in progress.
    sign_in: Mutex<Option<oneshot::Sender<()>>>,
    /// Folders (of any account) the UI wants in sync: on the sidebar or open.
    watched: Arc<Mutex<HashSet<i64>>>,
    /// Cancels the server search in progress.
    search: Notify,
}

struct Worker {
    shared: Arc<Shared>,
    task: JoinHandle<()>,
    /// Keeps IDLE on the inbox (`watch`).
    watch: JoinHandle<()>,
}

impl Worker {
    fn abort(&self) {
        self.task.abort();
        self.watch.abort();
    }
}

/// What a worker shares with its inbox's IDLE (`watch`) and the UI.
#[derive(Default)]
struct Shared {
    /// Ends the worker's wait for its next round.
    wake: Notify,
    asks: Asks,
    watcher: Watcher,
}

/// How the inbox's IDLE is doing.
#[derive(Default)]
struct Watcher {
    /// IDLE is on the inbox, so the worker checks it only with the sweep.
    watching: AtomicBool,
    /// A round found the server out of connections: IDLE gives its one up
    /// for `IDLE_RETRY_MAX`.
    give_way: AtomicBool,
    /// Tells IDLE to connect again, e.g. after the computer slept.
    restart: Notify,
}

/// What the user asked a worker for, between rounds.
#[derive(Default)]
struct Asks {
    /// A sweep, shown as syncing: the user asked, or the account was added.
    fetch: AtomicBool,
    /// The inbox, quietly: IDLE saw it change.
    inbox: AtomicBool,
    /// The inbox, quietly, unless it was checked within `INBOX_EVERY`:
    /// the window came to the front.
    focus: AtomicBool,
    list_folders: AtomicBool,
    /// Folders to sync on the next round, whatever the timer says.
    folders: Mutex<HashSet<i64>>,
    /// Reach further back in the inbox on the next round.
    older_inbox: AtomicBool,
    /// Folders to reach further back in on the next round.
    older_folders: Mutex<HashSet<i64>>,
    /// Told when the round that took the asks above is over. A waiter is
    /// added after its asks, and taken before them.
    older_waiters: Mutex<Vec<oneshot::Sender<()>>>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncEvent {
    account: i64,
    #[serde(flatten)]
    view: SyncView,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SendEvent {
    id: i64,
    account: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    draft_id: Option<i64>,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    /// The send failed on a server certificate the user may trust.
    #[serde(skip_serializing_if = "Option::is_none")]
    certificate: Option<Untrusted>,
}

impl Live {
    /// Opens (or creates) `mail.sqlite` in `data_dir`.
    pub fn open(data_dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(data_dir)
            .map_err(|e| format!("Couldn't create {}: {e}", data_dir.display()))?;
        let store = Store::open(&data_dir.join("mail.sqlite"))
            .map_err(|e| format!("Couldn't open the mail database: {e}"))?;
        // Sends cut off by a crash or quit go out on the next round.
        store
            .requeue_interrupted_sends()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            engine: Arc::new(
                Engine::new(
                    Arc::new(Mutex::new(store)),
                    Arc::new(MailSecrets::platform()),
                )
                .with_attachment_cache(data_dir.join("attachments")),
            ),
            workers: Mutex::new(HashMap::new()),
            status: Arc::new(Mutex::new(HashMap::new())),
            sign_in: Mutex::new(None),
            watched: Arc::new(Mutex::new(HashSet::new())),
            search: Notify::new(),
        })
    }

    pub fn has_accounts(&self) -> bool {
        self.engine.store().has_accounts().unwrap_or(false)
    }

    /// Starts a worker per account and the snooze timer.
    pub fn start_all(&self, app: &AppHandle) {
        let accounts = self.engine.store().accounts().unwrap_or_default();
        for account in accounts {
            self.start(app, account.id);
        }
        let engine = self.engine.clone();
        let status = self.status.clone();
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(SNOOZE_CHECK).await;
                let returned = engine.store().wake_snoozed(now_ms()).unwrap_or_default();
                // Reminders wait for a sync after their time, so a reply is seen first.
                let synced: HashMap<i64, i64> = status
                    .lock()
                    .unwrap()
                    .iter()
                    .filter_map(|(&account, view)| Some((account, view.last_sync?)))
                    .collect();
                let reminded = engine
                    .store()
                    .wake_reminders(now_ms(), &synced)
                    .unwrap_or_default();
                if !returned.is_empty() || !reminded.is_empty() {
                    let _ = app.emit("mailbox-changed", ());
                }
                if !returned.is_empty() {
                    notify::snooze_returned(&app, &engine, &returned);
                }
                notify::reminders_due(&app, &reminded);
            }
        });
    }

    /// Starts the account's worker; its first round fetches and lists the
    /// folders at once.
    pub fn start(&self, app: &AppHandle, account_id: i64) {
        let shared = Arc::new(Shared {
            asks: Asks {
                fetch: AtomicBool::new(true),
                list_folders: AtomicBool::new(true),
                ..Default::default()
            },
            ..Default::default()
        });
        let task = tauri::async_runtime::spawn(work(
            app.clone(),
            self.engine.clone(),
            self.status.clone(),
            self.watched.clone(),
            account_id,
            shared.clone(),
        ));
        let watch =
            tauri::async_runtime::spawn(watch(self.engine.clone(), account_id, shared.clone()));
        let old = self.workers.lock().unwrap().insert(
            account_id,
            Worker {
                shared,
                task,
                watch,
            },
        );
        if let Some(old) = old {
            old.abort();
        }
    }

    pub fn stop(&self, account_id: i64) {
        if let Some(worker) = self.workers.lock().unwrap().remove(&account_id) {
            worker.abort();
        }
        self.status.lock().unwrap().remove(&account_id);
    }

    /// Fetches new mail, lists folders and syncs the watched ones for every
    /// account now.
    pub fn sync_now(&self) {
        for worker in self.workers.lock().unwrap().values() {
            worker.shared.asks.fetch.store(true, Ordering::Relaxed);
            worker
                .shared
                .asks
                .list_folders
                .store(true, Ordering::Relaxed);
            worker.shared.wake.notify_one();
        }
    }

    /// The window came to the front: checks every inbox not checked
    /// within `INBOX_EVERY`, as Spark does.
    pub fn focused(&self) {
        for worker in self.workers.lock().unwrap().values() {
            worker.shared.asks.focus.store(true, Ordering::Relaxed);
            worker.shared.wake.notify_one();
        }
    }

    /// Lists every account's folders again now.
    pub fn refresh_folders(&self) {
        for worker in self.workers.lock().unwrap().values() {
            worker
                .shared
                .asks
                .list_folders
                .store(true, Ordering::Relaxed);
            worker.shared.wake.notify_one();
        }
    }

    /// Replaces the watched folders; ones not watched before sync now.
    pub fn watch_folders(&self, ids: Vec<i64>) {
        let ids: HashSet<i64> = ids.into_iter().collect();
        let added: Vec<i64> = {
            let mut watched = self.watched.lock().unwrap();
            let added = ids.difference(&watched).copied().collect();
            *watched = ids;
            added
        };
        self.sync_folders(added);
    }

    /// Syncs these folders on their accounts' next rounds, starting now.
    pub fn sync_folders(&self, ids: Vec<i64>) {
        if ids.is_empty() {
            return;
        }
        // Each engine round skips other accounts' folders.
        for worker in self.workers.lock().unwrap().values() {
            worker.shared.asks.folders.lock().unwrap().extend(&ids);
            worker.shared.wake.notify_one();
        }
    }

    /// Reaches further back in these accounts' inboxes and these folders
    /// (of any account) on their accounts' next rounds, starting now. Each
    /// receiver resolves, or is dropped, once the round of one of the
    /// accounts involved is over.
    pub fn load_older(&self, accounts: Vec<i64>, folders: Vec<i64>) -> Vec<oneshot::Receiver<()>> {
        let mut owned: HashMap<i64, Vec<i64>> = HashMap::new();
        {
            let store = self.engine.store();
            for id in folders {
                if let Ok(Some(folder)) = store.folder(id) {
                    owned.entry(folder.account_id).or_default().push(id);
                }
            }
        }
        let mut involved: HashSet<i64> = accounts.iter().copied().collect();
        involved.extend(owned.keys());
        let workers = self.workers.lock().unwrap();
        let mut done = Vec::new();
        for account_id in involved {
            let Some(worker) = workers.get(&account_id) else {
                continue;
            };
            if accounts.contains(&account_id) {
                worker
                    .shared
                    .asks
                    .older_inbox
                    .store(true, Ordering::Relaxed);
            }
            if let Some(ids) = owned.get(&account_id) {
                worker.shared.asks.older_folders.lock().unwrap().extend(ids);
            }
            let (tx, rx) = oneshot::channel();
            worker.shared.asks.older_waiters.lock().unwrap().push(tx);
            worker.shared.wake.notify_one();
            done.push(rx);
        }
        done
    }

    /// Puts a search to every account's server at once (or just `account`'s),
    /// storing the mail they find that isn't stored yet. A server that fails
    /// or takes longer than `SEARCH_WAIT` is left out. Returns how many
    /// messages were new, or `None` if a new search cancelled this one.
    pub async fn search_servers(&self, query: &str, account: Option<i64>) -> Option<usize> {
        self.search.notify_waiters();
        let cancelled = self.search.notified();
        tokio::pin!(cancelled);
        cancelled.as_mut().enable();
        let mut accounts = self.engine.store().accounts().unwrap_or_default();
        accounts.retain(|a| account.is_none_or(|id| a.id == id));
        let searches = join_all(accounts.iter().map(|account| async move {
            let search = self.engine.search_server(account.id, query);
            match tokio::time::timeout(SEARCH_WAIT, search).await {
                Ok(Ok(new)) => new,
                Ok(Err(e)) => {
                    eprintln!("spark-rs: searching account {}: {e}", account.id);
                    0
                }
                Err(_) => {
                    eprintln!("spark-rs: searching account {}: no answer", account.id);
                    0
                }
            }
        }));
        tokio::select! {
            new = searches => Some(new.into_iter().sum()),
            () = cancelled => None,
        }
    }

    /// Wakes the account's worker, e.g. for a send that's already due.
    pub fn nudge(&self, account_id: i64) {
        if let Some(worker) = self.workers.lock().unwrap().get(&account_id) {
            worker.shared.wake.notify_one();
        }
    }

    pub fn status(&self, account_id: i64) -> SyncView {
        self.status
            .lock()
            .unwrap()
            .get(&account_id)
            .cloned()
            .unwrap_or_default()
    }

    /// A receiver that resolves when `cancel_sign_in` is called. Starting a
    /// new sign-in cancels the previous one.
    pub fn sign_in_cancel(&self) -> oneshot::Receiver<()> {
        let (tx, rx) = oneshot::channel();
        if let Some(previous) = self.sign_in.lock().unwrap().replace(tx) {
            let _ = previous.send(());
        }
        rx
    }

    pub fn cancel_sign_in(&self) {
        if let Some(tx) = self.sign_in.lock().unwrap().take() {
            let _ = tx.send(());
        }
    }
}

async fn work(
    app: AppHandle,
    engine: Arc<Engine>,
    status: Arc<Mutex<HashMap<i64, SyncView>>>,
    watched: Arc<Mutex<HashSet<i64>>>,
    account_id: i64,
    shared: Arc<Shared>,
) {
    let Shared {
        wake,
        asks,
        watcher,
    } = &*shared;
    let set_status = |view: SyncView| {
        status.lock().unwrap().insert(account_id, view.clone());
        let _ = app.emit(
            "sync-status",
            SyncEvent {
                account: account_id,
                view,
            },
        );
    };
    let mut last_fetch: Option<Instant> = None;
    let mut last_sweep: Option<Instant> = None;
    let mut last_sync: Option<i64> = None;
    let mut retry_at: Option<Instant> = None;
    let mut slept = false;
    loop {
        if !matches!(engine.store().account(account_id), Ok(Some(_))) {
            break;
        }

        match engine.send_due(account_id).await {
            Ok(outcomes) => {
                for outcome in outcomes {
                    let event = match outcome.result {
                        Ok(true) => SendEvent {
                            id: outcome.id,
                            account: account_id,
                            draft_id: outcome.draft_id,
                            status: "sent",
                            message: None,
                            certificate: None,
                        },
                        Ok(false) => continue,
                        Err(e) => SendEvent {
                            id: outcome.id,
                            account: account_id,
                            draft_id: outcome.draft_id,
                            status: "failed",
                            message: Some(e.to_string()),
                            certificate: certificate_of(&e),
                        },
                    };
                    let _ = app.emit("send-status", event);
                }
            }
            Err(e) => eprintln!("spark-rs: sending for account {account_id}: {e}"),
        }

        // Waiters first: their asks are in by then.
        let waiters = std::mem::take(&mut *asks.older_waiters.lock().unwrap());
        let requested = asks.fetch.swap(false, Ordering::Relaxed);
        let inbox_asked = asks.inbox.swap(false, Ordering::Relaxed);
        let focused = asks.focus.swap(false, Ordering::Relaxed);
        let list_asked = asks.list_folders.swap(false, Ordering::Relaxed);
        let mut folders: HashSet<i64> = std::mem::take(&mut *asks.folders.lock().unwrap());
        let older_inbox = asks.older_inbox.swap(false, Ordering::Relaxed);
        let older_folders = std::mem::take(&mut *asks.older_folders.lock().unwrap());
        let due = due(
            last_fetch.map(|at| at.elapsed()),
            last_sweep.map(|at| at.elapsed()),
            watcher.watching.load(Ordering::Relaxed),
            focused,
        );
        let woke = std::mem::take(&mut slept);
        let sweep = requested || woke || due.sweep;
        let fetch = sweep || inbox_asked || due.inbox;
        let asked = requested
            || woke
            || inbox_asked
            || (focused && due.inbox)
            || list_asked
            || !folders.is_empty()
            || older_inbox
            || !older_folders.is_empty()
            || !waiters.is_empty();
        let waiting = retry_at.is_some_and(|at| Instant::now() < at);
        if asked || !waiting {
            let list_folders = list_asked || sweep;
            if sweep {
                folders.extend(watched.lock().unwrap().iter());
            }
            // Checks on the schedule don't spin the sync button; ones the
            // user asked for and the first do.
            if fetch && (requested || last_sync.is_none()) {
                set_status(SyncView {
                    status: SyncStatus::Syncing,
                    last_sync,
                    ..Default::default()
                });
            }
            let round = Round {
                fetch,
                repair: sweep,
                list_folders,
                folders: folders.into_iter().collect(),
                older_inbox,
                older_folders: older_folders.into_iter().collect(),
            };
            // Only inbox syncs move it.
            let had_older = (fetch || older_inbox).then(|| has_older(&engine, account_id));
            let folders_were = (!round.folders.is_empty() || !round.older_folders.is_empty())
                .then(|| folder_reach(&engine, account_id));
            match engine.run_round(account_id, &round).await {
                Ok(report) => {
                    retry_at = None;
                    if sweep {
                        last_sweep = Some(Instant::now());
                    }
                    if fetch {
                        last_fetch = Some(Instant::now());
                        last_sync = Some(now_ms());
                        set_status(SyncView {
                            status: SyncStatus::Idle,
                            last_sync,
                            ..Default::default()
                        });
                    }
                    for e in &report.folder_errors {
                        eprintln!("spark-rs: syncing a folder of account {account_id}: {e}");
                    }
                    if report.folders_changed {
                        let _ = app.emit("folders-changed", ());
                    }
                    if report.changed() {
                        let _ = app.emit("mailbox-changed", ());
                    }
                    if !report.arrived.is_empty() {
                        notify::new_mail(&app, &engine, account_id, &report.arrived);
                    }
                }
                Err(e) => {
                    retry_at = Some(Instant::now() + RETRY_AFTER);
                    last_fetch = Some(Instant::now());
                    // IDLE's connection may be the one too many.
                    if e.is_connection_limit() && watcher.watching.load(Ordering::Relaxed) {
                        watcher.give_way.store(true, Ordering::Relaxed);
                        watcher.restart.notify_one();
                    }
                    set_status(SyncView {
                        status: SyncStatus::Error,
                        message: Some(describe(&e)),
                        last_sync,
                        certificate: certificate_of(&e),
                    });
                    // A folder synced before it failed may have changed what the
                    // sidebar shows of it, which a report would have said.
                    if folders_were.is_some_and(|were| were != folder_reach(&engine, account_id)) {
                        let _ = app.emit("folders-changed", ());
                    }
                }
            }
            // The account's `hasOlder` flipped, e.g. with its first sync.
            if had_older.is_some_and(|had| had != has_older(&engine, account_id)) {
                let _ = app.emit("accounts-changed", ());
            }
            for waiter in waiters {
                let _ = waiter.send(());
            }
        }

        let waited_from = now_ms();
        tokio::select! {
            _ = wake.notified() => {}
            _ = tokio::time::sleep(TICK) => {}
        }
        // Waking from sleep: IDLE's connection is gone, and mail came in.
        if now_ms() - waited_from > (TICK + SLEPT).as_millis() as i64 {
            slept = true;
            watcher.restart.notify_one();
        }
    }
}

/// What a worker's schedule has due.
#[derive(Debug, PartialEq, Eq)]
struct Due {
    inbox: bool,
    sweep: bool,
}

/// From how long ago the inbox was last fetched and the last sweep ran
/// (`None`: not yet), whether IDLE watches the inbox, and whether the window
/// just came to the front.
fn due(
    since_inbox: Option<Duration>,
    since_sweep: Option<Duration>,
    watching: bool,
    focused: bool,
) -> Due {
    let past = |since: Option<Duration>, every| since.is_none_or(|since| since >= every);
    let sweep = past(since_sweep, SWEEP_EVERY);
    // IDLE tells when the inbox changes, so then only the sweep checks it.
    let every = if watching { SWEEP_EVERY } else { INBOX_EVERY };
    let inbox = sweep || past(since_inbox, every) || (focused && past(since_inbox, INBOX_EVERY));
    Due { inbox, sweep }
}

/// Keeps IDLE on the account's inbox, so the worker fetches new mail as it
/// arrives. Ends for accounts it can't watch; the worker checks their inbox
/// every `INBOX_EVERY` instead, as it does while IDLE is down.
async fn watch(engine: Arc<Engine>, account_id: i64, shared: Arc<Shared>) {
    let Shared {
        wake,
        asks,
        watcher,
    } = &*shared;
    let mut retry = IDLE_RETRY;
    loop {
        let started = Instant::now();
        let watched = engine
            .watch_inbox(account_id, &watcher.restart, &watcher.watching, || {
                asks.inbox.store(true, Ordering::Relaxed);
                wake.notify_one();
            })
            .await;
        watcher.watching.store(false, Ordering::Relaxed);
        // One that ran a while didn't fail the way the last one did.
        if started.elapsed() >= SWEEP_EVERY {
            retry = IDLE_RETRY;
        }
        let wait = match watched {
            Ok(Watch::Unwatched) => return,
            Ok(Watch::Restart) if watcher.give_way.swap(false, Ordering::Relaxed) => IDLE_RETRY_MAX,
            Ok(Watch::Restart) => continue,
            Err(e) if e.is_connection_limit() => IDLE_RETRY_MAX,
            Err(e) => {
                eprintln!("spark-rs: watching the inbox of account {account_id}: {e}");
                let wait = retry;
                retry = (retry * 2).min(IDLE_RETRY_MAX);
                wait
            }
        };
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = watcher.restart.notified() => {}
        }
    }
}

fn has_older(engine: &Engine, account_id: i64) -> bool {
    matches!(engine.store().account(account_id), Ok(Some(a)) if inbox_has_older(&a))
}

/// What the UI shows of an account's folder syncs (`FolderView`): whether
/// each folder synced yet, and has older mail to reach back for.
fn folder_reach(engine: &Engine, account_id: i64) -> Vec<(i64, bool, bool)> {
    let folders = engine.store().folders().unwrap_or_default();
    folders
        .iter()
        .filter(|f| f.account_id == account_id)
        .map(|f| (f.id, f.synced_at.is_some(), folder_has_older(f)))
        .collect()
}

/// Network errors are usually just being offline.
fn describe(e: &Error) -> String {
    match e {
        Error::Network { .. } => format!("Offline or the server isn't answering. {e}"),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Option<Duration> {
        Some(Duration::from_secs(n))
    }

    #[test]
    fn the_first_round_sweeps() {
        let due = due(None, None, false, false);
        assert!(due.inbox && due.sweep);
    }

    #[test]
    fn an_unwatched_inbox_is_checked_every_30_seconds() {
        assert_eq!(
            due(secs(29), secs(29), false, false),
            Due {
                inbox: false,
                sweep: false
            }
        );
        assert_eq!(
            due(secs(30), secs(30), false, false),
            Due {
                inbox: true,
                sweep: false
            }
        );
    }

    #[test]
    fn a_watched_inbox_waits_for_the_sweep() {
        assert_eq!(
            due(secs(299), secs(299), true, false),
            Due {
                inbox: false,
                sweep: false
            }
        );
        assert_eq!(
            due(secs(300), secs(300), true, false),
            Due {
                inbox: true,
                sweep: true
            }
        );
    }

    #[test]
    fn the_sweep_runs_every_five_minutes() {
        assert_eq!(
            due(secs(10), secs(300), false, false),
            Due {
                inbox: true,
                sweep: true
            }
        );
    }

    #[test]
    fn coming_to_the_front_checks_an_inbox_not_checked_just_now() {
        assert_eq!(
            due(secs(29), secs(29), true, true),
            Due {
                inbox: false,
                sweep: false
            }
        );
        assert_eq!(
            due(secs(30), secs(30), true, true),
            Due {
                inbox: true,
                sweep: false
            }
        );
    }
}
