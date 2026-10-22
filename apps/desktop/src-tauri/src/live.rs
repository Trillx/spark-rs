//! Real accounts: the mail engine and one background worker per account.
//!
//! A worker loops: send what's due, push local changes once they're past the
//! undo window, and fetch new mail every couple of minutes or when asked.
//! It tells the UI through events (`mailbox-changed`, `sync-status`,
//! `send-status`), and the user through notifications for new mail.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use spark_mail::{Engine, Error};
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

pub struct Live {
    pub engine: Arc<Engine>,
    workers: Mutex<HashMap<i64, Worker>>,
    status: Arc<Mutex<HashMap<i64, SyncView>>>,
    /// Cancels the Microsoft sign-in in progress.
    sign_in: Mutex<Option<oneshot::Sender<()>>>,
}

struct Worker {
    wake: Arc<Notify>,
    fetch_now: Arc<AtomicBool>,
    task: JoinHandle<()>,
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

    /// Starts the account's worker; its first round fetches at once.
    pub fn start(&self, app: &AppHandle, account_id: i64) {
        let wake = Arc::new(Notify::new());
        let fetch_now = Arc::new(AtomicBool::new(true));
        let task = tauri::async_runtime::spawn(work(
            app.clone(),
            self.engine.clone(),
            self.status.clone(),
            account_id,
            wake.clone(),
            fetch_now.clone(),
        ));
        let old = self.workers.lock().unwrap().insert(
            account_id,
            Worker {
                wake,
                fetch_now,
                task,
            },
        );
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

    /// Fetches new mail for every account now.
    pub fn sync_now(&self) {
        for worker in self.workers.lock().unwrap().values() {
            worker.fetch_now.store(true, Ordering::Relaxed);
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
    account_id: i64,
    wake: Arc<Notify>,
    fetch_now: Arc<AtomicBool>,
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

        let requested = fetch_now.swap(false, Ordering::Relaxed);
        let waiting = retry_at.is_some_and(|at| Instant::now() < at);
        if requested || !waiting {
            let fetch = requested || last_fetch.is_none_or(|at| at.elapsed() >= FETCH_EVERY);
            if fetch {
                set_status(SyncView {
                    status: SyncStatus::Syncing,
                    message: None,
                    last_sync,
                });
            }
            match engine.run(account_id, fetch).await {
                Ok(report) => {
                    retry_at = None;
                    if fetch {
                        last_fetch = Some(Instant::now());
                        last_sync = Some(now_ms());
                        set_status(SyncView {
                            status: SyncStatus::Idle,
                            message: None,
                            last_sync,
                        });
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
