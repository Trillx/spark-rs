//! `mailto:` links, and spark-rs as Windows' email app. Windows starts
//! spark-rs with the link as an argument; a second launch hands it to the
//! one running. The window opens a new email with it (`lib/mailto.ts`).
//!
//! Windows only lets the user choose the default app, so Make Default
//! registers spark-rs as an email app (for the current user) and opens
//! Default apps at its page.

use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::background;

/// The link spark-rs was given and the window hasn't taken yet.
#[derive(Default)]
pub struct Pending(Mutex<Option<String>>);

/// The first `mailto:` link among a launch's arguments.
pub fn link_in<S: AsRef<str>>(args: impl IntoIterator<Item = S>) -> Option<String> {
    args.into_iter()
        .map(|arg| arg.as_ref().trim().to_owned())
        .find(|arg| {
            arg.get(..7)
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case("mailto:"))
        })
}

/// Keeps the link spark-rs was started with for the window.
pub fn started_with(app: &AppHandle) {
    if let Some(link) = link_in(std::env::args().skip(1)) {
        *app.state::<Pending>().0.lock().unwrap() = Some(link);
    }
}

/// A link from a second launch: the window comes forward and takes it.
pub fn received(app: &AppHandle, link: String) {
    *app.state::<Pending>().0.lock().unwrap() = Some(link);
    background::show(app);
    let _ = app.emit("mailto", ());
}

/// The link waiting for the window, once.
#[tauri::command]
pub fn take_mailto(pending: State<'_, Pending>) -> Option<String> {
    pending.0.lock().unwrap().take()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MailApp {
    /// Windows can make spark-rs its email app.
    pub supported: bool,
    /// It is: email links open here.
    pub default: bool,
}

#[tauri::command]
pub fn mail_app() -> MailApp {
    MailApp {
        supported: cfg!(windows),
        default: is_default(),
    }
}

/// Registers spark-rs as an email app and opens Windows' Default apps at
/// it, where the user makes it the default.
#[tauri::command]
pub fn make_default_mail_app() -> Result<(), String> {
    register()?;
    let page = format!("ms-settings:defaultapps?registeredAppUser={APP}");
    open::that_detached(&page)
        .or_else(|_| open::that_detached("ms-settings:defaultapps"))
        .map_err(|e| format!("Couldn't open Default apps: {e}"))
}

/// Keeps a registration that's there pointing at this copy of spark-rs,
/// which an update may have moved.
pub fn refresh() {
    if registered()
        && let Err(e) = register()
    {
        eprintln!("spark-rs: {e}");
    }
}

/// The name Windows lists spark-rs under, and its link handler's ProgID.
const APP: &str = "spark-rs";
#[cfg_attr(not(windows), allow(dead_code))]
const PROG_ID: &str = "spark-rs.mailto";

#[cfg(windows)]
fn register() -> Result<(), String> {
    use windows_registry::CURRENT_USER;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.display();
    let capabilities = format!(r"Software\Clients\Mail\{APP}\Capabilities");
    let result = (|| {
        let prog = CURRENT_USER.create(format!(r"Software\Classes\{PROG_ID}"))?;
        prog.set_string("", "spark-rs email link")?;
        prog.set_string("URL Protocol", "")?;
        CURRENT_USER
            .create(format!(r"Software\Classes\{PROG_ID}\DefaultIcon"))?
            .set_string("", format!("\"{exe}\",0"))?;
        CURRENT_USER
            .create(format!(r"Software\Classes\{PROG_ID}\shell\open\command"))?
            .set_string("", format!("\"{exe}\" \"%1\""))?;
        CURRENT_USER
            .create(format!(r"Software\Clients\Mail\{APP}"))?
            .set_string("", APP)?;
        CURRENT_USER
            .create(format!(r"Software\Clients\Mail\{APP}\shell\open\command"))?
            .set_string("", format!("\"{exe}\""))?;
        let caps = CURRENT_USER.create(&capabilities)?;
        caps.set_string("ApplicationName", APP)?;
        caps.set_string(
            "ApplicationDescription",
            "Email with Spark's smart inbox, for Windows",
        )?;
        CURRENT_USER
            .create(format!(r"{capabilities}\URLAssociations"))?
            .set_string("mailto", PROG_ID)?;
        CURRENT_USER
            .create(r"Software\RegisteredApplications")?
            .set_string(APP, &capabilities)
    })();
    result.map_err(|e| format!("Couldn't register spark-rs as an email app: {e}"))
}

#[cfg(not(windows))]
fn register() -> Result<(), String> {
    Err("Making spark-rs the email app is only available on Windows for now.".into())
}

#[cfg(windows)]
fn registered() -> bool {
    windows_registry::CURRENT_USER
        .open(format!(r"Software\Classes\{PROG_ID}"))
        .is_ok()
}

#[cfg(not(windows))]
fn registered() -> bool {
    false
}

/// Whether the user chose spark-rs for email links.
#[cfg(windows)]
fn is_default() -> bool {
    windows_registry::CURRENT_USER
        .open(r"Software\Microsoft\Windows\Shell\Associations\UrlAssociations\mailto\UserChoice")
        .and_then(|key| key.get_string("ProgId"))
        .is_ok_and(|id| id.eq_ignore_ascii_case(PROG_ID))
}

#[cfg(not(windows))]
fn is_default() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_link_is_found_among_the_arguments() {
        assert_eq!(
            link_in(["--background", " MAILTO:ana@x.com?subject=Hi "]).as_deref(),
            Some("MAILTO:ana@x.com?subject=Hi")
        );
        assert_eq!(link_in(["--background", "mailto"]), None);
        assert_eq!(link_in(Vec::<String>::new()), None);
    }
}
