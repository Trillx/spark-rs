//! Real accounts: the mail engine and one background worker per account.
//!
//! A worker loops: send what's due, push local changes once they're past the
//! undo window, and fetch new mail every couple of minutes or when asked,
//! syncing the folders the UI watches along with it. The folder list is
//! refreshed now and then. It tells the UI through events
//! (`mailbox-changed`, `folders-changed`, `sync-status`, `send-status`), and
//! the user through notifications for new mail.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use spark_mail::{Engine, Error, Round};
use spark_store::{Store, now_ms};
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter};
use tokio::sync::{Notify, oneshot};

use crate::mailbox::{SyncStatus, SyncView};
use crate::notify;
use crate::secrets::MailSecrets;

const TICK: Duration = Duration::from_secs(2);
const FETCH_EVERY: Duration = Duration::from_secs(120);
const RETRY_AFTER: Duration = Duration::from_secs(30);
const SNOOZE_CHECK: Duration = Duration::from_secs(15);
const LIST_FOLDERS_EVERY: Duration = Duration::from_secs(15 * 60);

pub struct Live {
    pub engine: Arc<Engine>,
    workers: Mutex<HashMap<i64, Worker>>,
    status: Arc<Mutex<HashMap<i64, SyncView>>>,
    /// Cancels the Microsoft sign-in in progress.
    sign_in: Mutex<Option<oneshot::Sender<()>>>,
    /// Folders (of any account) the UI wants in sync: on the sidebar or open.
    watched: Arc<Mutex<HashSet<i64>>>,
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
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
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
                if engine.store().wake_snoozed(now_ms()).unwrap_or(0) > 0 {
                    let _ = app.emit("mailbox-changed", ());
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
                            status: "sent",
                            message: None,
                        },
                        Ok(false) => continue,
                        Err(e) => SendEvent {
                            id: outcome.id,
                            status: "failed",
                            message: Some(e.to_string()),
                        },
                    };
                    let _ = app.emit("send-status", event);
                }
            }
            Err(e) => eprintln!("spark-rs: sending for account {account_id}: {e}"),
        }

        let requested = asks.fetch.swap(false, Ordering::Relaxed);
        let list_asked = asks.list_folders.swap(false, Ordering::Relaxed);
        let mut folders: HashSet<i64> = std::mem::take(&mut *asks.folders.lock().unwrap());
        let asked = requested || list_asked || !folders.is_empty();
        let waiting = retry_at.is_some_and(|at| Instant::now() < at);
        if asked || !waiting {
            let fetch = requested || last_fetch.is_none_or(|at| at.elapsed() >= FETCH_EVERY);
            let list_folders = list_asked
                || (fetch && last_list.is_none_or(|at| at.elapsed() >= LIST_FOLDERS_EVERY));
            if fetch {
                folders.extend(watched.lock().unwrap().iter());
                set_status(SyncView {
                    status: SyncStatus::Syncing,
                    message: None,
                    last_sync,
                });
            }
            let round = Round {
                fetch,
                list_folders,
                folders: folders.into_iter().collect(),
            };
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
                            message: None,
                            last_sync,
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
                    });
                }
            }
        }

        tokio::select! {
            _ = wake.notified() => {}
            _ = tokio::time::sleep(TICK) => {}
        }
    }
}

/// Network errors are usually just being offline.
fn describe(e: &Error) -> String {
    match e {
        Error::Network { .. } => format!("Offline or the server isn't answering. {e}"),
        other => other.to_string(),
    }
}
