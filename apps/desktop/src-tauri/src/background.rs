//! Running in the background: the icon in the notification area, closing the
//! window to it, and opening at sign-in. While the window is hidden the sync
//! workers keep going and new mail shows as notifications (`notify.rs`).

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{App, AppHandle, Manager, Window, WindowEvent};

use crate::mailbox::Mailbox;
use crate::notify;
use crate::prefs::PrefsState;

/// Passed by the sign-in entry: start with the window hidden.
const BACKGROUND_ARG: &str = "--background";

/// Adds the tray icon and shows the window, unless started at sign-in.
pub fn setup(app: &App) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open spark-rs", true, None::<&str>)?;
    let sync = MenuItem::with_id(app, "sync", "Check for new mail", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit spark-rs", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[&open, &sync, &PredefinedMenuItem::separator(app)?, &quit],
    )?;
    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip(&app.package_info().name)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show(app),
            "sync" => app.state::<Mailbox>().live.sync_now(),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;

    // The sign-in entry names this exe; a rebuilt or moved app updates it.
    if app.state::<PrefsState>().get().open_at_login
        && let Err(e) = set_open_at_login(true)
    {
        eprintln!("spark-rs: {e}");
    }
    if !started_in_background(std::env::args()) {
        show(app.handle());
    }
    Ok(())
}

/// For a launch while spark-rs is already running.
pub fn second_launch(app: &AppHandle, args: Vec<String>) {
    if !started_in_background(args) {
        show(app);
    }
}

fn started_in_background<S: AsRef<str>>(args: impl IntoIterator<Item = S>) -> bool {
    args.into_iter().any(|arg| arg.as_ref() == BACKGROUND_ARG)
}

/// Brings the window back from the tray, the taskbar or behind other windows.
pub fn show(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Whether the user is looking at the window, so new mail needs no
/// notification.
pub fn in_front(app: &AppHandle) -> bool {
    app.get_webview_window("main").is_some_and(|w| {
        w.is_visible().unwrap_or(false)
            && w.is_focused().unwrap_or(false)
            && !w.is_minimized().unwrap_or(true)
    })
}

/// Closing the window hides it when Settings → Notifications says to keep
/// running. The first time, a notification says where the app went.
/// Otherwise it quits, even with the meeting pop-up open.
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    if window.label() != "main" {
        return;
    }
    if let WindowEvent::Destroyed = event {
        window.app_handle().exit(0);
        return;
    }
    let WindowEvent::CloseRequested { api, .. } = event else {
        return;
    };
    let prefs = window.state::<PrefsState>();
    if !prefs.get().keep_running {
        return;
    }
    api.prevent_close();
    let _ = window.hide();
    if !prefs.get().told_keep_running {
        notify::kept_running(window.app_handle());
        if let Err(e) = prefs.update(|p| p.told_keep_running = true) {
            eprintln!("spark-rs: {e}");
        }
    }
}

/// Adds or removes spark-rs from the apps Windows starts at sign-in.
#[cfg(windows)]
pub fn set_open_at_login(on: bool) -> Result<(), String> {
    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const NAME: &str = "spark-rs";
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let result = windows_registry::CURRENT_USER.create(RUN).and_then(|key| {
        if on {
            key.set_string(NAME, format!("\"{}\" {BACKGROUND_ARG}", exe.display()))
        } else if key.get_string(NAME).is_ok() {
            key.remove_value(NAME)
        } else {
            Ok(())
        }
    });
    result.map_err(|e| format!("Couldn't change whether spark-rs opens at sign-in: {e}"))
}

#[cfg(not(windows))]
pub fn set_open_at_login(on: bool) -> Result<(), String> {
    if on {
        Err("Opening at sign-in is only available on Windows for now.".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sign_in_entry_starts_hidden() {
        assert!(started_in_background([r"C:\spark-rs.exe", "--background"]));
        assert!(!started_in_background([r"C:\spark-rs.exe"]));
    }
}
