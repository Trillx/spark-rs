//! Running in the background: the icon in the notification area, closing the
//! window to it, opening at sign-in, and the badge on the taskbar button.
//! While the window is hidden the sync workers keep going and new mail shows
//! as notifications (`notify.rs`).

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{App, AppHandle, Emitter, Manager, WebviewWindow, Window, WindowEvent};

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
            "quit" => crate::thread_window::quit(app),
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
    let args: Vec<S> = args.into_iter().collect();
    // An email link's launch shows the window, whatever else its arguments say.
    crate::mailto::link_in(&args).is_none() && args.iter().any(|arg| arg.as_ref() == BACKGROUND_ARG)
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
/// Otherwise it quits, even with the meeting pop-up open, once the
/// conversations' windows have saved their drafts and closed.
///
/// Any spark-rs window gaining or losing focus (the meeting pop-up too)
/// tells the main window, whose Home Screen counts time away from all of
/// them, as Spark does; coming back to any of them takes down the Email
/// Focus reminder. The main window coming to the front also checks the
/// inboxes, as in Spark.
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    if let WindowEvent::Focused(focused) = event {
        let _ = window
            .app_handle()
            .emit_to("main", "window-focus", *focused);
        if *focused {
            notify::withdraw_focus_reminder(window.app_handle());
            if window.label() == "main" {
                window.state::<Mailbox>().live.focused();
            }
        }
        return;
    }
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
        api.prevent_close();
        crate::thread_window::quit(window.app_handle());
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
        // A link can't hide the window by naming the flag.
        assert!(!started_in_background([
            r"C:\spark-rs.exe",
            "mailto:a@x.com",
            "--background"
        ]));
    }
}

/// Shows `count` on the app's taskbar button; 0 takes it away. Windows has
/// no count badge, so there the UI draws one (32px square, RGBA) for the
/// button's overlay icon.
#[tauri::command]
pub fn set_badge(window: WebviewWindow, count: u32, icon: Option<Vec<u8>>) -> Result<(), String> {
    #[cfg(windows)]
    {
        const BADGE_SIZE: u32 = 32;
        let size = (BADGE_SIZE * BADGE_SIZE * 4) as usize;
        let icon = icon
            .filter(|rgba| count > 0 && rgba.len() == size)
            .map(|rgba| tauri::image::Image::new_owned(rgba, BADGE_SIZE, BADGE_SIZE));
        window.set_overlay_icon(icon).map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        let _ = icon;
        let count = (count > 0).then_some(i64::from(count));
        window.set_badge_count(count).map_err(|e| e.to_string())
    }
}
