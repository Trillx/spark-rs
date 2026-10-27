//! Spark's "Open Thread in a new Window": a conversation in a window of its
//! own. It's the app's page again (`index.html`), told which conversation
//! it's for by a script that runs before the page (`window.__sparkThread`);
//! it shows that conversation alone, and closes when it's done with it.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{
    AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder, Window,
    WindowEvent,
};

use crate::background;

/// Set while quitting waits for the conversations' windows to close.
static QUITTING: AtomicBool = AtomicBool::new(false);

fn is_thread_window(label: &str) -> bool {
    label.starts_with("thread-")
}

/// Whether conversations' windows are open, besides the one labelled `but`.
fn others_open(app: &AppHandle, but: &str) -> bool {
    app.webview_windows()
        .keys()
        .any(|label| is_thread_window(label) && label != but)
}

/// Quits spark-rs once the conversations' windows have closed. Each saves
/// its draft and queues its sends as it does for its own close button; one
/// that can't stays open (its error says why), and so does spark-rs.
pub fn quit(app: &AppHandle) {
    let windows: Vec<_> = app
        .webview_windows()
        .into_iter()
        .filter(|(label, _)| is_thread_window(label))
        .map(|(_, window)| window)
        .collect();
    if windows.is_empty() {
        app.exit(0);
        return;
    }
    QUITTING.store(true, Ordering::SeqCst);
    for window in windows {
        let _ = window.close();
    }
}

/// A window closing gives up the draft it had open. The last conversation's
/// window closing finishes a quit that waited for it.
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    if !matches!(event, WindowEvent::Destroyed) {
        return;
    }
    if let Some(editing) = window.try_state::<Editing>() {
        editing.0.lock().unwrap().remove(window.label());
    }
    if is_thread_window(window.label())
        && QUITTING.load(Ordering::SeqCst)
        && !others_open(window.app_handle(), window.label())
    {
        window.app_handle().exit(0);
    }
}

/// The saved draft each window's composer has open, by window label. A
/// draft is edited in one window at a time, so that two can't each save
/// their own copy over the other's changes.
#[derive(Default)]
pub struct Editing(Mutex<HashMap<String, i64>>);

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

/// A conversation's window stayed open when asked to close (its draft
/// didn't save): a quit waiting for it stops.
#[tauri::command]
pub fn kept_open() {
    QUITTING.store(false, Ordering::SeqCst);
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
