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

use futures_util::FutureExt;
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
/// A wait this much longer than it should be, by the wall clock, means the
/// computer slept.
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
    /// Notices the computer waking from sleep (`notice_waking`).
    waking: JoinHandle<()>,
}

impl Worker {
    fn abort(&self) {
        self.task.abort();
        self.watch.abort();
        self.waking.abort();
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
    /// A sweep, quietly: the computer woke from sleep.
    woke: AtomicBool,
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
        let waking = tauri::async_runtime::spawn(notice_waking(shared.clone(), TICK, now_ms));
        let old = self.workers.lock().unwrap().insert(
            account_id,
            Worker {
                shared,
                task,
                watch,
                waking,
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
    let mut schedule = Schedule::default();
    let mut last_sync: Option<i64> = None;
    loop {
        if !matches!(engine.store().account(account_id), Ok(Some(_))) {
            break;
        }

        // Answered once it's gone, not when it was queued (Undo and Send
        // Later can still take it back), and before the next one goes.
        let answered = |outcome: &spark_mail::engine::SendOutcome| {
            if matches!(outcome.result, Ok(true))
                && let Some(original) = outcome.reply_to
            {
                crate::automation::replied(&app, original);
            }
        };
        match engine.send_due_each(account_id, answered).await {
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
        let woke = asks.woke.swap(false, Ordering::Relaxed);
        let list_asked = asks.list_folders.swap(false, Ordering::Relaxed);
        let mut folders: HashSet<i64> = std::mem::take(&mut *asks.folders.lock().unwrap());
        let older_inbox = asks.older_inbox.swap(false, Ordering::Relaxed);
        let older_folders = std::mem::take(&mut *asks.older_folders.lock().unwrap());
        let wanted = Wanted {
            sweep: requested,
            inbox: inbox_asked,
            focused,
            woke,
            other: list_asked
                || !folders.is_empty()
                || older_inbox
                || !older_folders.is_empty()
                || !waiters.is_empty(),
        };
        let watching = watcher.watching.load(Ordering::Relaxed);
        if let Some(plan) = schedule.plan(Instant::now(), watching, &wanted) {
            let Plan { fetch, sweep } = plan;
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
            let result = engine.run_round(account_id, &round).await;
            let ended = match &result {
                Ok(report) if report.skipped > 0 => Ended::Short,
                Ok(_) => Ended::Whole,
                Err(_) => Ended::Failed,
            };
            schedule.finished(Instant::now(), &plan, ended);
            match result {
                Ok(report) => {
                    if fetch {
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
                        crate::automation::wake(&app);
                    }
                }
                Err(e) => {
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

        tokio::select! {
            _ = wake.notified() => {}
            _ = tokio::time::sleep(TICK) => {}
        }
    }
}

/// Notices the computer waking from sleep: a wait of `tick` that took over
/// `SLEPT` longer by `wall` (now, in ms). IDLE's connection is gone then,
/// and mail came in, so IDLE is told to connect again and the worker to
/// sweep. Beside the worker, not in its wait: a sleep during one of its
/// rounds, which can take minutes, counts too. (Timers can't tell on their
/// own: on Windows they count the time asleep, and elsewhere they don't.)
async fn notice_waking(shared: Arc<Shared>, tick: Duration, wall: impl Fn() -> i64) {
    loop {
        let from = wall();
        tokio::time::sleep(tick).await;
        if wall() - from > (tick + SLEPT).as_millis() as i64 {
            shared.asks.woke.store(true, Ordering::Relaxed);
            shared.watcher.restart.notify_one();
            shared.wake.notify_one();
        }
    }
}

/// A worker's timetable: when it last fetched the inbox and swept, and
/// what a failed or cut-short round left to do.
#[derive(Default)]
struct Schedule {
    last_fetch: Option<Instant>,
    last_sweep: Option<Instant>,
    /// After a failed round, rounds nothing asked for wait until then.
    retry_at: Option<Instant>,
    /// An inbox fetch a round failed or the server cut short, which rounds
    /// from then make up.
    owed: Option<Instant>,
    /// Inbox fetches in a row the server cut short.
    short: u32,
}

/// What a round was asked for, besides the schedule.
#[derive(Default)]
struct Wanted {
    /// The user asked for a sweep.
    sweep: bool,
    /// IDLE saw the inbox change.
    inbox: bool,
    /// The window came to the front.
    focused: bool,
    /// The computer woke from sleep.
    woke: bool,
    /// Folders, the folder list, older mail.
    other: bool,
}

/// How a round went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ended {
    Whole,
    /// It worked, but the server wouldn't give some new inbox mail.
    Short,
    Failed,
}

/// What a round does besides pushing local changes.
#[derive(Debug, PartialEq, Eq)]
struct Plan {
    fetch: bool,
    sweep: bool,
}

impl Schedule {
    /// The next round, or `None` while it waits after a failure. `watching`:
    /// IDLE is on the inbox.
    fn plan(&mut self, now: Instant, watching: bool, wanted: &Wanted) -> Option<Plan> {
        let past = |at: Option<Instant>, every| at.is_none_or(|at| now - at >= every);
        let sweep = wanted.sweep || wanted.woke || past(self.last_sweep, SWEEP_EVERY);
        // IDLE tells when the inbox changes, so then only the sweep checks it.
        let every = if watching { SWEEP_EVERY } else { INBOX_EVERY };
        let focus = wanted.focused && past(self.last_fetch, INBOX_EVERY);
        let owed = self.owed.is_some_and(|at| now >= at);
        let fetch = sweep || wanted.inbox || focus || owed || past(self.last_fetch, every);
        let asked = wanted.sweep || wanted.woke || wanted.inbox || focus || wanted.other;
        let waiting = self.retry_at.is_some_and(|at| now < at);
        (asked || !waiting).then_some(Plan { fetch, sweep })
    }

    fn finished(&mut self, now: Instant, plan: &Plan, ended: Ended) {
        if ended == Ended::Failed {
            self.retry_at = Some(now + RETRY_AFTER);
            if plan.fetch {
                self.owed = Some(now);
            }
        } else {
            self.retry_at = None;
            if plan.sweep {
                self.last_sweep = Some(now);
            }
            if plan.fetch && ended == Ended::Short {
                // The mail held back is asked for again `INBOX_EVERY` on,
                // then half as often each time the server still won't give
                // it, until the sweep's five minutes.
                let wait = INBOX_EVERY.saturating_mul(2u32.saturating_pow(self.short));
                self.owed = Some(now + wait.min(SWEEP_EVERY));
                self.short = self.short.saturating_add(1);
            } else if plan.fetch {
                self.owed = None;
                self.short = 0;
            }
        }
        // A failed fetch waits as long as one that worked.
        if plan.fetch || ended == Ended::Failed {
            self.last_fetch = Some(now);
        }
    }
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
        // However the watch ended, a round out of connections asked IDLE to
        // give its one up, and the restart that asked doesn't end the wait.
        let give_way = watcher.give_way.swap(false, Ordering::Relaxed);
        if give_way {
            let _ = watcher.restart.notified().now_or_never();
        }
        if let Err(e) = &watched
            && !e.is_connection_limit()
        {
            eprintln!("spark-rs: watching the inbox of account {account_id}: {e}");
        }
        let wait = match next_watch(&watched, give_way, &mut retry) {
            None => return,
            Some(Duration::ZERO) => continue,
            Some(wait) => wait,
        };
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = watcher.restart.notified() => {}
        }
    }
}

/// How long after a watch ended the next one starts: `None` for an account
/// it can't watch, zero for one told to start over. `retry` is the wait
/// after a failure, which doubles.
fn next_watch(
    watched: &Result<Watch, Error>,
    give_way: bool,
    retry: &mut Duration,
) -> Option<Duration> {
    Some(match watched {
        Ok(Watch::Unwatched) => return None,
        _ if give_way => IDLE_RETRY_MAX,
        Ok(Watch::Restart) => Duration::ZERO,
        Err(e) if e.is_connection_limit() => IDLE_RETRY_MAX,
        Err(_) => {
            let wait = *retry;
            *retry = (*retry * 2).min(IDLE_RETRY_MAX);
            wait
        }
    })
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
    use std::sync::atomic::AtomicI64;

    use super::*;

    /// A schedule that swept at `start`, and the time `n` seconds later.
    fn swept() -> (Schedule, impl Fn(u64) -> Instant) {
        let start = Instant::now();
        let at = move |n| start + Duration::from_secs(n);
        let mut schedule = Schedule::default();
        let plan = schedule.plan(at(0), false, &Wanted::default()).unwrap();
        assert_eq!(
            plan,
            Plan {
                fetch: true,
                sweep: true
            },
            "the first round sweeps"
        );
        schedule.finished(at(0), &plan, Ended::Whole);
        (schedule, at)
    }

    const QUIET: Plan = Plan {
        fetch: false,
        sweep: false,
    };
    const INBOX: Plan = Plan {
        fetch: true,
        sweep: false,
    };
    const SWEEP: Plan = Plan {
        fetch: true,
        sweep: true,
    };

    #[test]
    fn an_unwatched_inbox_is_checked_every_30_seconds() {
        let (mut schedule, at) = swept();
        let nothing = Wanted::default();
        assert_eq!(schedule.plan(at(29), false, &nothing), Some(QUIET));
        assert_eq!(schedule.plan(at(30), false, &nothing), Some(INBOX));
    }

    #[test]
    fn a_watched_inbox_waits_for_the_sweep_every_five_minutes() {
        let (mut schedule, at) = swept();
        let nothing = Wanted::default();
        assert_eq!(schedule.plan(at(299), true, &nothing), Some(QUIET));
        assert_eq!(schedule.plan(at(300), true, &nothing), Some(SWEEP));
    }

    #[test]
    fn coming_to_the_front_checks_an_inbox_not_checked_just_now() {
        let (mut schedule, at) = swept();
        let focused = Wanted {
            focused: true,
            ..Default::default()
        };
        assert_eq!(schedule.plan(at(29), true, &focused), Some(QUIET));
        assert_eq!(schedule.plan(at(30), true, &focused), Some(INBOX));
    }

    #[test]
    fn a_failed_fetch_is_made_up_after_the_wait() {
        let (mut schedule, at) = swept();
        let changed = Wanted {
            inbox: true,
            ..Default::default()
        };
        let plan = schedule.plan(at(10), true, &changed).unwrap();
        assert_eq!(plan, INBOX);
        schedule.finished(at(10), &plan, Ended::Failed);
        assert_eq!(schedule.plan(at(20), true, &Wanted::default()), None);
        let plan = schedule.plan(at(40), true, &Wanted::default()).unwrap();
        assert_eq!(plan, INBOX);
        schedule.finished(at(40), &plan, Ended::Whole);
        assert_eq!(schedule.plan(at(45), true, &Wanted::default()), Some(QUIET));
    }

    #[test]
    fn waking_from_sleep_sweeps_even_while_waiting() {
        let (mut schedule, at) = swept();
        let plan = schedule.plan(at(10), false, &Wanted::default()).unwrap();
        schedule.finished(at(10), &plan, Ended::Failed);
        let woke = Wanted {
            woke: true,
            ..Default::default()
        };
        assert_eq!(schedule.plan(at(11), false, &woke), Some(SWEEP));
    }

    /// The computer sleeping during a round (here a sweep, past its inbox
    /// fetch) is noticed though the round ends after it: IDLE connects
    /// again, and the next round sweeps once more. The wall clock keeping
    /// time with the timers, however long the round takes, isn't sleep.
    #[tokio::test]
    async fn sleeping_during_a_round_restarts_idle_and_sweeps_after_it() {
        let shared = Arc::new(Shared::default());
        let asleep = Arc::new(AtomicI64::new(0));
        let wall = {
            let asleep = asleep.clone();
            move || now_ms() + asleep.load(Ordering::Relaxed)
        };
        let tick = Duration::from_millis(10);
        let waking = tokio::spawn(notice_waking(shared.clone(), tick, wall));
        let (mut schedule, at) = swept();
        let plan = schedule.plan(at(300), true, &Wanted::default()).unwrap();
        assert_eq!(plan, SWEEP);

        tokio::time::sleep(tick * 20).await;
        assert!(!shared.asks.woke.load(Ordering::Relaxed));
        // An hour asleep.
        asleep.store(60 * 60 * 1000, Ordering::Relaxed);
        let restarted =
            tokio::time::timeout(Duration::from_secs(5), shared.watcher.restart.notified()).await;
        waking.abort();
        assert!(restarted.is_ok(), "IDLE wasn't told to connect again");

        schedule.finished(at(301), &plan, Ended::Whole);
        let wanted = Wanted {
            woke: shared.asks.woke.load(Ordering::Relaxed),
            ..Default::default()
        };
        assert_eq!(schedule.plan(at(303), true, &wanted), Some(SWEEP));
    }

    /// New mail the server wouldn't give (as Gmail does for a while) is
    /// asked for again 30 seconds on, not five minutes on with the sweep.
    #[test]
    fn an_inbox_cut_short_is_checked_again_after_30_seconds() {
        let (mut schedule, at) = swept();
        let changed = Wanted {
            inbox: true,
            ..Default::default()
        };
        let plan = schedule.plan(at(10), true, &changed).unwrap();
        assert_eq!(plan, INBOX);
        schedule.finished(at(10), &plan, Ended::Short);
        let nothing = Wanted::default();
        assert_eq!(schedule.plan(at(39), true, &nothing), Some(QUIET));
        let plan = schedule.plan(at(40), true, &nothing).unwrap();
        assert_eq!(plan, INBOX);
        schedule.finished(at(40), &plan, Ended::Whole);
        assert_eq!(schedule.plan(at(299), true, &nothing), Some(QUIET));
    }

    /// Mail the server never gives is asked for half as often each time,
    /// then only with the sweep, every five minutes.
    #[test]
    fn mail_the_server_keeps_refusing_is_asked_for_less_and_less_often() {
        let (mut schedule, at) = swept();
        let changed = Wanted {
            inbox: true,
            ..Default::default()
        };
        let plan = schedule.plan(at(10), true, &changed).unwrap();
        schedule.finished(at(10), &plan, Ended::Short);
        let mut fetched = Vec::new();
        for n in 11..=1000 {
            let plan = schedule.plan(at(n), true, &Wanted::default()).unwrap();
            let ended = if plan.fetch {
                fetched.push(n);
                Ended::Short
            } else {
                Ended::Whole
            };
            schedule.finished(at(n), &plan, ended);
        }
        assert_eq!(fetched, [40, 100, 220, 300, 600, 900]);
    }

    #[test]
    fn idle_gives_way_however_the_watch_ended() {
        let mut retry = IDLE_RETRY;
        let failed: Result<Watch, Error> = Err(Error::Server(
            "The mail server closed the connection.".into(),
        ));
        assert_eq!(next_watch(&failed, true, &mut retry), Some(IDLE_RETRY_MAX));
        assert_eq!(
            next_watch(&Ok(Watch::Restart), true, &mut retry),
            Some(IDLE_RETRY_MAX)
        );
        assert_eq!(retry, IDLE_RETRY);
    }

    #[test]
    fn a_failed_watch_waits_longer_each_time() {
        let mut retry = IDLE_RETRY;
        let failed: Result<Watch, Error> = Err(Error::Server(
            "The mail server closed the connection.".into(),
        ));
        let waits: Vec<_> = (0..6)
            .map(|_| next_watch(&failed, false, &mut retry).unwrap())
            .collect();
        let secs: Vec<u64> = waits.iter().map(Duration::as_secs).collect();
        assert_eq!(secs, [30, 60, 120, 240, 300, 300]);
        let full: Result<Watch, Error> = Err(Error::Auth(
            "[LIMIT] Too many simultaneous connections".into(),
        ));
        assert_eq!(next_watch(&full, false, &mut retry), Some(IDLE_RETRY_MAX));
        assert_eq!(
            next_watch(&Ok(Watch::Restart), false, &mut retry),
            Some(Duration::ZERO)
        );
        assert_eq!(next_watch(&Ok(Watch::Unwatched), false, &mut retry), None);
    }
}
