//! Spark's "Open Thread in a new Window": a conversation in a window of its
//! own. It's the app's page again (`index.html`), told which conversation
//! it's for by a script that runs before the page (`window.__sparkThread`);
//! it shows that conversation alone, and closes when it's done with it.

use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::background;

/// Opens conversation `id` (titled `title`) in its own window, or brings
/// its window forward if it has one. Async, as making a window from a
/// synchronous command can deadlock on Windows.
#[tauri::command]
pub async fn open_thread_window(app: AppHandle, id: i64, title: String) -> Result<(), String> {
    let label = format!("thread-{id}");
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(());
    }
    let title = title.trim();
    WebviewWindowBuilder::new(&app, &label, WebviewUrl::App("index.html".into()))
        .initialization_script(format!("window.__sparkThread = {id};"))
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
