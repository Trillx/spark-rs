//! Spark's "Open Thread in a new Window": a conversation in a window of its
//! own. It's the app's page again (`index.html`), told which conversation
//! it's for by a script that runs before the page (`window.__sparkThread`);
//! it shows that conversation alone, and closes when it's done with it.

use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::background;

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
