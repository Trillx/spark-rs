//! New-mail notifications: Windows toasts with the New Mail sound. Clicking
//! one brings the window forward and opens the message (`open-message`).
//!
//! An app without an installer has to introduce itself to Windows before its
//! toasts show: `register` writes its name and icon under
//! `HKCU\Software\Classes\AppUserModelId\<identifier>`.

use spark_mail::Engine;
use spark_store::{Category, Message, MessageState};
use tauri::{AppHandle, Manager};

use crate::background;
use crate::prefs::{NotifyFor, PrefsState};

/// More new mail than this in one sync becomes a single "N new emails".
const MAX_SEPARATE: usize = 3;

/// One toast.
#[derive(Debug, PartialEq, Eq)]
pub struct Alert {
    pub title: String,
    /// Up to two lines under the title.
    pub lines: Vec<String>,
    /// Small print at the bottom: the account, when there's more than one.
    pub attribution: Option<String>,
    /// The message a click opens.
    pub message: Option<i64>,
}

/// Introduces the app to Windows' notification system. Run at every start:
/// it also follows the app's icon file if the data folder moved.
pub fn register(app: &AppHandle) {
    #[cfg(windows)]
    if let Err(e) = toast::register(app) {
        eprintln!("spark-rs: notifications may not show: {e}");
    }
    #[cfg(not(windows))]
    let _ = app;
}

/// Notifies about mail that just arrived in an account, as the user's
/// Settings → Notifications say. Quiet while the window is in front.
pub fn new_mail(app: &AppHandle, engine: &Engine, account_id: i64, ids: &[i64]) {
    let prefs = app.state::<PrefsState>().get();
    if prefs.notify == NotifyFor::Off || background::in_front(app) {
        return;
    }
    let (arrived, account) = {
        let store = engine.store();
        let arrived: Vec<Message> = ids
            .iter()
            .filter_map(|&id| store.message(id).ok().flatten())
            .collect();
        let several = store.accounts().map(|a| a.len() > 1).unwrap_or(false);
        let label = several
            .then(|| store.account(account_id).ok().flatten())
            .flatten()
            .map(|a| a.label);
        (arrived, label)
    };
    // One sound per sync, not one per toast.
    let mut sound = prefs.sound;
    for alert in alerts(&arrived, prefs.notify, account.as_deref()) {
        if let Err(e) = show(app, &alert, sound) {
            eprintln!("spark-rs: couldn't show a notification: {e}");
            return;
        }
        sound = false;
    }
}

/// Shown the first time closing the window leaves the app running.
pub fn kept_running(app: &AppHandle) {
    let alert = Alert {
        title: "spark-rs is still running".into(),
        lines: vec![
            "It keeps checking for mail. To quit, right-click its icon in the notification area."
                .into(),
        ],
        attribution: None,
        message: None,
    };
    if let Err(e) = show(app, &alert, false) {
        eprintln!("spark-rs: couldn't show a notification: {e}");
    }
}

/// An agent's email waits in the outbox; while the window isn't in front,
/// a notification says so, since only the app can undo it.
pub fn agent_sending(app: &AppHandle, agent: &str, to: &str, subject: &str) {
    if background::in_front(app) {
        return;
    }
    let alert = Alert {
        title: format!("{agent} is sending an email"),
        lines: vec![
            format!("To {to}: {subject}"),
            "Open spark-rs within 30 seconds to undo it.".into(),
        ],
        attribution: None,
        message: None,
    };
    let sound = app.state::<PrefsState>().get().sound;
    if let Err(e) = show(app, &alert, sound) {
        eprintln!("spark-rs: couldn't show a notification: {e}");
    }
}

/// Settings → Notifications → Send a test notification.
#[tauri::command]
pub fn test_notification(app: AppHandle) -> Result<(), String> {
    let sound = app.state::<PrefsState>().get().sound;
    let alert = Alert {
        title: "spark-rs".into(),
        lines: vec!["New mail will look like this.".into()],
        attribution: None,
        message: None,
    };
    show(&app, &alert, sound)
}

/// What to show for the mail that arrived in one sync of one account.
/// Mail from new senders waits in the Gatekeeper without a notification.
pub fn alerts(arrived: &[Message], notify: NotifyFor, account: Option<&str>) -> Vec<Alert> {
    let wanted: Vec<&Message> = arrived
        .iter()
        .filter(|m| {
            m.unread
                && m.state == MessageState::Inbox
                && !m.screening
                && match notify {
                    NotifyFor::People => m.category == Category::People,
                    NotifyFor::All => true,
                    NotifyFor::Off => false,
                }
        })
        .collect();
    let attribution = account.map(str::to_owned);
    if wanted.len() > MAX_SEPARATE {
        return vec![Alert {
            title: format!("{} new emails", wanted.len()),
            lines: vec![senders(&wanted)],
            attribution,
            message: None,
        }];
    }
    wanted
        .into_iter()
        .map(|m| Alert {
            title: sender(m).to_owned(),
            lines: [
                if m.subject.trim().is_empty() {
                    "(no subject)"
                } else {
                    m.subject.as_str()
                },
                m.snippet.as_str(),
            ]
            .into_iter()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_owned)
            .collect(),
            attribution: attribution.clone(),
            message: Some(m.id),
        })
        .collect()
}

fn sender(m: &Message) -> &str {
    if m.from_name.trim().is_empty() {
        &m.from_address
    } else {
        &m.from_name
    }
}

/// "From Ana Ruiz, Bob and 3 others".
fn senders(messages: &[&Message]) -> String {
    let mut names: Vec<&str> = Vec::new();
    for m in messages {
        let name = sender(m);
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let shown = names.len().min(2);
    let rest = names.len() - shown;
    let list = names[..shown].join(", ");
    match rest {
        0 => format!("From {list}"),
        1 => format!("From {list} and 1 other"),
        n => format!("From {list} and {n} others"),
    }
}

/// The toast as Windows' XML schema.
fn toast_xml(alert: &Alert, sound: bool) -> String {
    let mut texts = format!("<text>{}</text>", escape(&alert.title));
    for line in alert.lines.iter().take(2) {
        texts += &format!("<text>{}</text>", escape(line));
    }
    if let Some(attribution) = &alert.attribution {
        texts += &format!(
            r#"<text placement="attribution">{}</text>"#,
            escape(attribution)
        );
    }
    let audio = if sound {
        r#"<audio src="ms-winsoundevent:Notification.Mail"/>"#
    } else {
        r#"<audio silent="true"/>"#
    };
    format!(
        r#"<toast><visual><binding template="ToastGeneric">{texts}</binding></visual>{audio}</toast>"#
    )
}

/// Escapes text for XML, dropping the control characters XML can't carry
/// (they turn up in subjects).
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(' '),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

fn show(app: &AppHandle, alert: &Alert, sound: bool) -> Result<(), String> {
    #[cfg(windows)]
    return toast::show(app, &toast_xml(alert, sound), alert.message);
    #[cfg(not(windows))]
    {
        let _ = (app, toast_xml(alert, sound));
        Err("Notifications are only available on Windows for now.".into())
    }
}

#[cfg(windows)]
mod toast {
    use std::fs;
    use std::sync::Mutex;

    use tauri::{AppHandle, Emitter, Manager};
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::Foundation::TypedEventHandler;
    use windows::UI::Notifications::{
        NotificationSetting, ToastNotification, ToastNotificationManager,
    };
    use windows::core::HSTRING;

    use crate::background;

    const ICON: &[u8] = include_bytes!("../icons/128x128@2x.png");

    /// Recent toasts. Holding them keeps their click handlers alive while
    /// they wait in the notification center.
    static SHOWN: Mutex<Vec<ToastNotification>> = Mutex::new(Vec::new());
    const KEEP: usize = 20;

    pub fn register(app: &AppHandle) -> Result<(), String> {
        let dir = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
        let icon = dir.join("notification-icon.png");
        if fs::read(&icon).ok().as_deref() != Some(ICON) {
            fs::create_dir_all(&dir)
                .and_then(|()| fs::write(&icon, ICON))
                .map_err(|e| format!("couldn't write {}: {e}", icon.display()))?;
        }
        let key = windows_registry::CURRENT_USER
            .create(format!(
                r"Software\Classes\AppUserModelId\{}",
                app.config().identifier
            ))
            .and_then(|key| {
                key.set_string("DisplayName", &app.package_info().name)?;
                key.set_string("IconUri", icon.to_string_lossy())?;
                Ok(key)
            });
        key.map(drop)
            .map_err(|e| format!("couldn't register with Windows: {e}"))
    }

    pub fn show(app: &AppHandle, xml: &str, message: Option<i64>) -> Result<(), String> {
        let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(
            &app.config().identifier,
        ))
        .map_err(|e| e.to_string())?;
        match notifier.Setting() {
            Ok(NotificationSetting::Enabled) | Err(_) => {}
            Ok(_) => {
                return Err(
                    "Windows has notifications turned off for spark-rs. Turn them on in \
                     Windows Settings → System → Notifications."
                        .into(),
                );
            }
        }
        let doc = XmlDocument::new().map_err(|e| e.to_string())?;
        doc.LoadXml(&HSTRING::from(xml))
            .map_err(|e| e.to_string())?;
        let toast = ToastNotification::CreateToastNotification(&doc).map_err(|e| e.to_string())?;
        let clicked = app.clone();
        toast
            .Activated(&TypedEventHandler::new(move |_, _| {
                background::show(&clicked);
                if let Some(id) = message {
                    let _ = clicked.emit("open-message", id);
                }
                Ok(())
            }))
            .map_err(|e| e.to_string())?;
        notifier.Show(&toast).map_err(|e| e.to_string())?;

        let mut shown = SHOWN.lock().unwrap();
        if shown.len() == KEEP {
            shown.remove(0);
        }
        shown.push(toast);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: i64, from: &str, subject: &str) -> Message {
        Message {
            id,
            account_id: 1,
            category: Category::People,
            state: MessageState::Inbox,
            snoozed_until: None,
            unread: true,
            pinned: false,
            priority: false,
            screening: false,
            from_name: from.into(),
            from_address: format!("{}@x.com", from.to_lowercase()),
            subject: subject.into(),
            snippet: format!("About {subject}"),
            date: 0,
            attachment: None,
            folders: Vec::new(),
            folder: None,
            to: Vec::new(),
        }
    }

    #[test]
    fn one_toast_per_message_from_people() {
        let arrived = [
            message(1, "Ana", "Lunch?"),
            Message {
                category: Category::Newsletters,
                ..message(2, "News", "Weekly digest")
            },
        ];
        assert_eq!(
            alerts(&arrived, NotifyFor::People, None),
            [Alert {
                title: "Ana".into(),
                lines: vec!["Lunch?".into(), "About Lunch?".into()],
                attribution: None,
                message: Some(1),
            }]
        );
        assert_eq!(alerts(&arrived, NotifyFor::All, Some("Work")).len(), 2);
        assert!(alerts(&arrived, NotifyFor::Off, None).is_empty());
    }

    #[test]
    fn skips_mail_already_read_screened_or_blocked() {
        let arrived = [
            Message {
                unread: false,
                ..message(1, "Ana", "Read elsewhere")
            },
            Message {
                screening: true,
                ..message(2, "Stranger", "Hi there")
            },
            Message {
                state: MessageState::Blocked,
                ..message(3, "Spam", "Win")
            },
        ];
        assert!(alerts(&arrived, NotifyFor::All, None).is_empty());
    }

    #[test]
    fn many_at_once_become_one_toast() {
        let arrived: Vec<Message> = ["Ana", "Bob", "Ana", "Cy", "Dee"]
            .into_iter()
            .enumerate()
            .map(|(i, from)| message(i as i64, from, "Hello"))
            .collect();
        assert_eq!(
            alerts(&arrived, NotifyFor::People, Some("Work")),
            [Alert {
                title: "5 new emails".into(),
                lines: vec!["From Ana, Bob and 2 others".into()],
                attribution: Some("Work".into()),
                message: None,
            }]
        );
    }

    #[test]
    fn falls_back_to_the_address_and_a_placeholder_subject() {
        let arrived = [Message {
            from_name: String::new(),
            snippet: String::new(),
            ..message(1, "Ana", " ")
        }];
        let alert = &alerts(&arrived, NotifyFor::People, None)[0];
        assert_eq!(alert.title, "ana@x.com");
        assert_eq!(alert.lines, ["(no subject)"]);
    }

    #[test]
    fn toast_xml_escapes_text_and_picks_the_sound() {
        let alert = Alert {
            title: "Ana <ana@x.com>".into(),
            lines: vec!["Q&A\u{b}\ttomorrow".into()],
            attribution: Some("Work".into()),
            message: Some(1),
        };
        assert_eq!(
            toast_xml(&alert, true),
            r#"<toast><visual><binding template="ToastGeneric"><text>Ana &lt;ana@x.com&gt;</text><text>Q&amp;A tomorrow</text><text placement="attribution">Work</text></binding></visual><audio src="ms-winsoundevent:Notification.Mail"/></toast>"#
        );
        assert!(toast_xml(&alert, false).ends_with(r#"<audio silent="true"/></toast>"#));
    }
}
