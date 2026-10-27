//! Spark's "Open Thread in a new Window": a conversation in a window of its
//! own. It's the app's page again (`index.html`), told which conversation
//! it's for by a script that runs before the page (`window.__sparkThread`);
//! it shows that conversation alone, and closes when it's done with it.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use tauri::{
    AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder, Window,
    WindowEvent,
};

use crate::background;

/// Set while quitting waits for the app's windows to save their drafts.
static QUITTING: AtomicBool = AtomicBool::new(false);
/// Set, while quitting, once the main window has saved its draft.
static MAIN_SETTLED: AtomicBool = AtomicBool::new(false);
/// Counts quits asked for, so a wait that gives up is the latest one's.
static QUITS: AtomicU64 = AtomicU64::new(0);
/// How long quitting waits for windows that don't answer (one still
/// loading, or stuck); saving a draft takes a moment, as it's saved here.
const QUIT_WAIT: Duration = Duration::from_secs(15);

fn is_thread_window(label: &str) -> bool {
    label.starts_with("thread-")
}

/// Whether conversations' windows are open, besides the one labelled `but`.
fn others_open(app: &AppHandle, but: &str) -> bool {
    app.webview_windows()
        .keys()
        .any(|label| is_thread_window(label) && label != but)
}

/// Quits spark-rs once its windows have saved their drafts and queued
/// their sends: the main window when asked to ("quitting"), the
/// conversations' windows as they close, as for their own close buttons.
/// One that can't stays open (its error says why), and so does spark-rs.
pub fn quit(app: &AppHandle) {
    let run = QUITS.fetch_add(1, Ordering::SeqCst) + 1;
    QUITTING.store(true, Ordering::SeqCst);
    let main = app.get_webview_window("main").is_some();
    MAIN_SETTLED.store(!main, Ordering::SeqCst);
    if main {
        let _ = app.emit_to("main", "quitting", ());
    }
    for (label, window) in app.webview_windows() {
        if is_thread_window(&label) {
            let _ = window.close();
        }
    }
    finish_quit(app, "");
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(QUIT_WAIT).await;
        if QUITTING.load(Ordering::SeqCst) && QUITS.load(Ordering::SeqCst) == run {
            app.exit(0);
        }
    });
}

/// Quits if quitting waits for nothing more: the main window has saved, and
/// no conversation's window is open but `closing`.
fn finish_quit(app: &AppHandle, closing: &str) {
    if QUITTING.load(Ordering::SeqCst)
        && MAIN_SETTLED.load(Ordering::SeqCst)
        && !others_open(app, closing)
    {
        app.exit(0);
    }
}

/// The main window, asked as spark-rs quits, has saved its draft.
#[tauri::command]
pub fn settled_for_quit(app: AppHandle) {
    MAIN_SETTLED.store(true, Ordering::SeqCst);
    finish_quit(&app, "");
}

/// A window closing gives up the draft it had open. The last conversation's
/// window closing can finish a quit that waited for it.
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    if !matches!(event, WindowEvent::Destroyed) {
        return;
    }
    if let Some(editing) = window.try_state::<Editing>() {
        editing.0.lock().unwrap().remove(window.label());
    }
    if is_thread_window(window.label()) {
        finish_quit(window.app_handle(), window.label());
    }
}

/// The saved draft each window's composer has open, by window label. A
/// draft is edited in one window at a time, so that two can't each save
/// their own copy over the other's changes.
#[derive(Default)]
pub struct Editing(Mutex<HashMap<String, i64>>);

impl Editing {
    /// Whether any window's composer has saved draft `id` open.
    pub fn holds(&self, id: i64) -> bool {
        self.0.lock().unwrap().values().any(|open| *open == id)
    }
}

/// This window's composer now has saved draft `id` open (none: no saved
/// draft, or no composer).
#[tauri::command]
pub fn editing_draft(window: WebviewWindow, editing: State<'_, Editing>, id: Option<i64>) {
    let mut open = editing.0.lock().unwrap();
    match id {
        Some(id) => open.insert(window.label().to_owned(), id),
        None => open.remove(window.label()),
    };
}

/// Whether saved draft `id` is open in another window's composer; if it
/// is, that window comes forward.
#[tauri::command]
pub fn draft_open_elsewhere(window: WebviewWindow, editing: State<'_, Editing>, id: i64) -> bool {
    let holder = editing
        .0
        .lock()
        .unwrap()
        .iter()
        .find(|(label, open)| **open == id && *label != window.label())
        .map(|(label, _)| label.clone());
    let Some(other) = holder.and_then(|label| window.app_handle().get_webview_window(&label))
    else {
        return false;
    };
    let _ = other.unminimize();
    let _ = other.show();
    let _ = other.set_focus();
    true
}

/// A window kept its draft when asked to close or quit, as it didn't save:
/// a quit waiting for it stops. The main window, maybe hidden in the
/// background, comes forward with its error.
#[tauri::command]
pub fn kept_open(window: WebviewWindow) {
    QUITTING.store(false, Ordering::SeqCst);
    if window.label() == "main" {
        background::show(window.app_handle());
    }
}

/// What a conversation's window can be asked to do as it opens: an +AI
/// summary, in one of Spark's styles.
const THEN: &[&str] = &["short", "detailed", "actionPoints"];

/// Opens conversation `id` (titled `title`) in its own window, or brings
/// its window forward if it has one; with `then`, a summary style, the
/// window summarizes it too. Async, as making a window from a synchronous
/// command can deadlock on Windows.
#[tauri::command]
pub async fn open_thread_window(
    app: AppHandle,
    id: i64,
    title: String,
    then: Option<String>,
) -> Result<(), String> {
    let then = then.filter(|t| THEN.contains(&t.as_str()));
    let label = format!("thread-{id}");
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        if let Some(then) = then {
            let _ = app.emit_to(&label, "thread-then", then);
        }
        return Ok(());
    }
    let title = title.trim();
    // `then` is one of `THEN`, so it's safe in the script.
    let script = match &then {
        Some(then) => {
            format!("window.__sparkThread = {id}; window.__sparkThreadThen = \"{then}\";")
        }
        None => format!("window.__sparkThread = {id};"),
    };
    WebviewWindowBuilder::new(&app, &label, WebviewUrl::App("index.html".into()))
        .initialization_script(script)
        .title(if title.is_empty() { "spark-rs" } else { title })
        .inner_size(880.0, 780.0)
        .min_inner_size(560.0, 480.0)
        .decorations(false)
        .build()
        .map(|_| ())
        .map_err(|e| format!("Couldn't open the window: {e}"))
}

/// A search asked for in a conversation's own window (Search Emails by
/// Sender): the main window, which has the list, comes forward and does it.
#[tauri::command]
pub fn search_in_main(app: AppHandle, query: String) {
    background::show(&app);
    let _ = app.emit_to("main", "search-in-main", query);
}
