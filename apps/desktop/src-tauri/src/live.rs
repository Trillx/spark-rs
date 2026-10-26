//! Real accounts: the mail engine and one background worker per account.
//!
//! A worker loops: send what's due, push local changes once they're past the
//! undo window, and fetch new mail every couple of minutes or when asked,
//! syncing the folders the UI watches along with it. Older mail is reached
//! back for when the UI asks. The folder list is refreshed now and then.
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
use spark_mail::engine::inbox_has_older;
use spark_mail::{Engine, Error, Round, Untrusted};
use spark_store::{Store, now_ms};
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter};
use tokio::sync::{Notify, oneshot};

use crate::mailbox::{SyncStatus, SyncView, certificate_of};
use crate::notify;
use crate::secrets::MailSecrets;

const TICK: Duration = Duration::from_secs(2);
const FETCH_EVERY: Duration = Duration::from_secs(120);
const RETRY_AFTER: Duration = Duration::from_secs(30);
const SNOOZE_CHECK: Duration = Duration::from_secs(15);
const LIST_FOLDERS_EVERY: Duration = Duration::from_secs(15 * 60);
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
    wake: Arc<Notify>,
    asks: Arc<Asks>,
    task: JoinHandle<()>,
}

/// What the user asked a worker for, between rounds.
#[derive(Default)]
struct Asks {
    fetch: AtomicBool,
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
            engine: Arc::new(Engine::new(
                Arc::new(Mutex::new(store)),
                Arc::new(MailSecrets::platform()),
            )),
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
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(SNOOZE_CHECK).await;
                let returned = engine.store().wake_snoozed(now_ms()).unwrap_or_default();
                if !returned.is_empty() {
                    let _ = app.emit("mailbox-changed", ());
                    notify::snooze_returned(&app, &engine, &returned);
                }
            }
        });
    }

    /// Starts the account's worker; its first round fetches and lists the
    /// folders at once.
    pub fn start(&self, app: &AppHandle, account_id: i64) {
        let wake = Arc::new(Notify::new());
        let asks = Arc::new(Asks {
            fetch: AtomicBool::new(true),
            list_folders: AtomicBool::new(true),
            ..Default::default()
        });
        let task = tauri::async_runtime::spawn(work(
            app.clone(),
            self.engine.clone(),
            self.status.clone(),
            self.watched.clone(),
            account_id,
            wake.clone(),
            asks.clone(),
        ));
        let old = self
            .workers
            .lock()
            .unwrap()
            .insert(account_id, Worker { wake, asks, task });
        if let Some(old) = old {
            old.task.abort();
        }
    }

    pub fn stop(&self, account_id: i64) {
        if let Some(worker) = self.workers.lock().unwrap().remove(&account_id) {
            worker.task.abort();
        }
        self.status.lock().unwrap().remove(&account_id);
    }

    /// Fetches new mail, lists folders and syncs the watched ones for every
    /// account now.
    pub fn sync_now(&self) {
        for worker in self.workers.lock().unwrap().values() {
            worker.asks.fetch.store(true, Ordering::Relaxed);
            worker.asks.list_folders.store(true, Ordering::Relaxed);
            worker.wake.notify_one();
        }
    }

    /// Lists every account's folders again now.
    pub fn refresh_folders(&self) {
        for worker in self.workers.lock().unwrap().values() {
            worker.asks.list_folders.store(true, Ordering::Relaxed);
            worker.wake.notify_one();
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
            worker.asks.folders.lock().unwrap().extend(&ids);
            worker.wake.notify_one();
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
                worker.asks.older_inbox.store(true, Ordering::Relaxed);
            }
            if let Some(ids) = owned.get(&account_id) {
                worker.asks.older_folders.lock().unwrap().extend(ids);
            }
            let (tx, rx) = oneshot::channel();
            worker.asks.older_waiters.lock().unwrap().push(tx);
            worker.wake.notify_one();
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
            worker.wake.notify_one();
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
    wake: Arc<Notify>,
    asks: Arc<Asks>,
) {
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
    let mut last_list: Option<Instant> = None;
    let mut last_sync: Option<i64> = None;
    let mut retry_at: Option<Instant> = None;
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
        let list_asked = asks.list_folders.swap(false, Ordering::Relaxed);
        let mut folders: HashSet<i64> = std::mem::take(&mut *asks.folders.lock().unwrap());
        let older_inbox = asks.older_inbox.swap(false, Ordering::Relaxed);
        let older_folders = std::mem::take(&mut *asks.older_folders.lock().unwrap());
        let asked = requested
            || list_asked
            || !folders.is_empty()
            || older_inbox
            || !older_folders.is_empty()
            || !waiters.is_empty();
        let waiting = retry_at.is_some_and(|at| Instant::now() < at);
        if asked || !waiting {
            let fetch = requested || last_fetch.is_none_or(|at| at.elapsed() >= FETCH_EVERY);
            let list_folders = list_asked
                || (fetch && last_list.is_none_or(|at| at.elapsed() >= LIST_FOLDERS_EVERY));
            if fetch {
                folders.extend(watched.lock().unwrap().iter());
                set_status(SyncView {
                    status: SyncStatus::Syncing,
                    last_sync,
                    ..Default::default()
                });
            }
            let round = Round {
                fetch,
                list_folders,
                folders: folders.into_iter().collect(),
                older_inbox,
                older_folders: older_folders.into_iter().collect(),
            };
            // Only inbox syncs move it.
            let had_older = (fetch || older_inbox).then(|| has_older(&engine, account_id));
            match engine.run_round(account_id, &round).await {
                Ok(report) => {
                    retry_at = None;
                    if list_folders {
                        last_list = Some(Instant::now());
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
                    set_status(SyncView {
                        status: SyncStatus::Error,
                        message: Some(describe(&e)),
                        last_sync,
                        certificate: certificate_of(&e),
                    });
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

        tokio::select! {
            _ = wake.notified() => {}
            _ = tokio::time::sleep(TICK) => {}
        }
    }
}

fn has_older(engine: &Engine, account_id: i64) -> bool {
    matches!(engine.store().account(account_id), Ok(Some(a)) if inbox_has_older(&a))
}

/// Network errors are usually just being offline.
fn describe(e: &Error) -> String {
    match e {
        Error::Network { .. } => format!("Offline or the server isn't answering. {e}"),
        other => other.to_string(),
    }
}
