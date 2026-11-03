//! New-mail notifications: Windows toasts with the New Mail sound. Clicking
//! one brings the window forward and opens the message (`open-message`).
//! Mail with a sign-in code (`spark_mail::sign_in_code`) gets a button that
//! copies the code without opening anything. Calendar alerts (`alerts.rs`)
//! show here too, with buttons.
//!
//! An app without an installer has to introduce itself to Windows before its
//! toasts show: `register` writes its name and icon under
//! `HKCU\Software\Classes\AppUserModelId\<identifier>`.

use std::sync::atomic::{AtomicBool, Ordering};

use spark_mail::Engine;
use spark_store::{
    Category, GatekeeperMode, Message, MessageState, Patch, Reminder, Store, now_ms,
};
use tauri::{AppHandle, Emitter, Manager};

use crate::background;
use crate::mailbox::Mailbox;
use crate::prefs::{AfterCodeCopy, NotifyFor, PrefsState};

/// More new mail than this in one sync becomes a single "N new emails".
const MAX_SEPARATE: usize = 3;

/// How long after it arrives a sign-in code is worth a notification of its
/// own: they last minutes, and mail that comes in late (a laptop waking up)
/// is past it.
const CODE_FRESH_MS: i64 = 60 * 60 * 1000;

/// The arguments of a toast's Copy button.
const COPY_CODE: &str = "copy-code";

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
    /// Its sign-in code, which a button copies.
    pub code: Option<String>,
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
        // Inside the inbox, a new sender's mail is inbox mail like any other.
        let inside = store.gatekeeper_mode().ok() == Some(GatekeeperMode::InsideInbox);
        let arrived: Vec<Message> = ids
            .iter()
            .filter_map(|&id| store.message(id).ok().flatten())
            .map(|m| Message {
                screening: m.screening && !inside,
                ..m
            })
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
    let codes = prefs.code_alerts.then_some(now_ms());
    for alert in alerts(&arrived, prefs.notify, codes, account.as_deref()) {
        if let Err(e) = show(app, &alert, sound) {
            eprintln!("spark-rs: couldn't show a notification: {e}");
            return;
        }
        sound = false;
    }
}

/// Snoozed mail is back in the Inbox: one notification per message, or one
/// for all if there are many, unless Settings → Notifications turned them
/// off. Quiet while the window is in front, where the list shows it.
pub fn snooze_returned(app: &AppHandle, engine: &Engine, ids: &[i64]) {
    let prefs = app.state::<PrefsState>().get();
    if !prefs.snooze_alert || background::in_front(app) {
        return;
    }
    // Not the ones that woke into Blocked: their sender was blocked meanwhile.
    let returned: Vec<Message> = {
        let store = engine.store();
        ids.iter()
            .filter_map(|&id| store.message(id).ok().flatten())
            .filter(|m| m.state == MessageState::Inbox)
            .collect()
    };
    // One sound for all of them, as for new mail.
    let mut sound = prefs.sound;
    for alert in returned_alerts(&returned) {
        if let Err(e) = show(app, &alert, sound) {
            eprintln!("spark-rs: couldn't show a notification: {e}");
            return;
        }
        sound = false;
    }
}

/// Reminders that came due: Spark's reminder, with the subject and who it
/// went to; a click opens the email it came back as.
pub fn reminders_due(app: &AppHandle, due: &[Reminder]) {
    if due.is_empty() || background::in_front(app) {
        return;
    }
    let mut sound = app.state::<PrefsState>().get().sound;
    for alert in reminder_alerts(due) {
        if let Err(e) = show(app, &alert, sound) {
            eprintln!("spark-rs: couldn't show a notification: {e}");
            return;
        }
        sound = false;
    }
}

pub fn reminder_alerts(due: &[Reminder]) -> Vec<Alert> {
    if due.len() > MAX_SEPARATE {
        return vec![Alert {
            title: format!("{} reminders", due.len()),
            lines: vec!["Their emails are back in your Inbox.".into()],
            attribution: None,
            message: None,
            code: None,
        }];
    }
    due.iter()
        .map(|r| Alert {
            title: if r.if_no_reply {
                "No reply yet"
            } else {
                "Reminder"
            }
            .into(),
            lines: vec![
                if r.subject.trim().is_empty() {
                    "(no subject)".to_owned()
                } else {
                    r.subject.clone()
                },
                format!("To {}", r.to),
            ],
            attribution: None,
            message: r.message,
            code: None,
        })
        .collect()
}

/// Spark's "Snoozed message returned to your Inbox", with sender and subject:
/// one per conversation (its newest email), as the Inbox shows them.
pub fn returned_alerts(returned: &[Message]) -> Vec<Alert> {
    let mut newest: Vec<&Message> = Vec::new();
    for m in returned {
        match newest
            .iter_mut()
            .find(|x| x.account_id == m.account_id && x.conversation == m.conversation)
        {
            Some(kept) if kept.date < m.date => *kept = m,
            Some(_) => {}
            None => newest.push(m),
        }
    }
    let returned = newest;
    if returned.len() > MAX_SEPARATE {
        return vec![Alert {
            title: format!("{} snoozed emails returned to your Inbox", returned.len()),
            lines: vec![senders(&returned)],
            attribution: None,
            message: None,
            code: None,
        }];
    }
    returned
        .into_iter()
        .map(|m| Alert {
            title: "Snoozed message returned to your Inbox".into(),
            lines: vec![
                sender(m).to_owned(),
                if m.subject.trim().is_empty() {
                    "(no subject)".into()
                } else {
                    m.subject.clone()
                },
            ],
            attribution: None,
            message: Some(m.id),
            code: None,
        })
        .collect()
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
        code: None,
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
        code: None,
    };
    let sound = app.state::<PrefsState>().get().sound;
    if let Err(e) = show(app, &alert, sound) {
        eprintln!("spark-rs: couldn't show a notification: {e}");
    }
}

/// The Email Focus reminder's tag: a new one replaces the last, and coming
/// back takes it down.
const FOCUS_TAG: &str = "email-focus";

/// An Email Focus reminder is up and hasn't been taken down.
static FOCUS_SHOWN: AtomicBool = AtomicBool::new(false);

/// An Email Focus time with Remind me came while spark-rs isn't in front
/// (Settings → Home Screen). Clicking it brings back the window, on the Home
/// Screen.
#[tauri::command]
pub fn focus_reminder(app: AppHandle, new_emails: u32) -> Result<(), String> {
    if background::in_front(&app) {
        return Ok(());
    }
    let sound = app.state::<PrefsState>().get().sound;
    show_tagged(&app, &focus_alert(new_emails), sound, Some(FOCUS_TAG))?;
    FOCUS_SHOWN.store(true, Ordering::Relaxed);
    Ok(())
}

/// spark-rs is back in front, so the Email Focus reminder has done its job:
/// take it down, as Spark does.
pub fn withdraw_focus_reminder(app: &AppHandle) {
    if !FOCUS_SHOWN.swap(false, Ordering::Relaxed) {
        return;
    }
    #[cfg(windows)]
    if let Err(e) = toast::remove(app, FOCUS_TAG) {
        eprintln!("spark-rs: couldn't take down the Email Focus reminder: {e}");
    }
    #[cfg(not(windows))]
    let _ = app;
}

/// Spark's wording for the Email Focus reminder.
pub fn focus_alert(new_emails: u32) -> Alert {
    let line = match new_emails {
        0 => "There are no new emails".to_string(),
        1 => "You've got 1 new email".to_string(),
        n => format!("You've got {n} new emails"),
    };
    Alert {
        title: "It's time for Email Focus!".into(),
        lines: vec![line],
        attribution: None,
        message: None,
        code: None,
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
        code: None,
    };
    show(&app, &alert, sound)
}

/// What to show for the mail that arrived in one sync of one account.
/// Mail waiting in New Senders (`screening`) or blocked shows none. With
/// `codes` (the time now), mail with a fresh sign-in code shows anyway, from
/// any sender but a muted conversation's, in a toast of its own: it's mail
/// the user is waiting for, often from a service they're signing up to,
/// whose sender is new.
pub fn alerts(
    arrived: &[Message],
    notify: NotifyFor,
    codes: Option<i64>,
    account: Option<&str>,
) -> Vec<Alert> {
    let fresh_code = |m: &Message| {
        notify != NotifyFor::Off
            && m.sign_in_code.is_some()
            && codes.is_some_and(|now| now - m.date < CODE_FRESH_MS)
    };
    let (mut with_codes, wanted): (Vec<&Message>, Vec<&Message>) = arrived
        .iter()
        .filter(|m| {
            m.unread
                && m.state == MessageState::Inbox
                && !m.muted
                && (fresh_code(m)
                    || !m.screening
                        && match notify {
                            NotifyFor::People => m.category == Category::People,
                            NotifyFor::All => true,
                            NotifyFor::Priority => m.priority,
                            NotifyFor::Off => false,
                        })
        })
        .partition(|m| fresh_code(m));
    // The newest few, the newest last so its toast is on top.
    with_codes.sort_by_key(|m| (m.date, m.id));
    let with_codes = &with_codes[with_codes.len().saturating_sub(MAX_SEPARATE)..];
    let mut shown = if wanted.len() > MAX_SEPARATE {
        vec![Alert {
            title: format!("{} new emails", wanted.len()),
            lines: vec![senders(&wanted)],
            attribution: account.map(str::to_owned),
            message: None,
            code: None,
        }]
    } else {
        wanted.into_iter().map(|m| one_alert(m, account)).collect()
    };
    shown.extend(with_codes.iter().map(|m| one_alert(m, account)));
    shown
}

/// A toast for one new email: who, the subject and its start; a button for
/// its sign-in code.
fn one_alert(m: &Message, attribution: Option<&str>) -> Alert {
    let subject = if m.subject.trim().is_empty() {
        "(no subject)"
    } else {
        m.subject.as_str()
    };
    Alert {
        title: sender(m).to_owned(),
        lines: [subject, m.snippet.as_str()]
            .into_iter()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_owned)
            .collect(),
        attribution: attribution.map(str::to_owned),
        message: Some(m.id),
        code: m.sign_in_code.clone(),
    }
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
    // Foreground, so the click comes to this process, which copies without
    // showing the window.
    let actions = match (&alert.code, alert.message) {
        (Some(code), Some(_)) => format!(
            r#"<actions><action activationType="foreground" arguments="{COPY_CODE}" content="Copy {}"/></actions>"#,
            escape(code)
        ),
        _ => String::new(),
    };
    format!(
        r#"<toast><visual><binding template="ToastGeneric">{texts}</binding></visual>{actions}{audio}</toast>"#
    )
}

/// Escapes text for XML, dropping the control characters XML can't carry
/// (they turn up in subjects).
pub fn escape(text: &str) -> String {
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

/// A click on a toast or one of its buttons.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Click {
    /// The button's arguments; empty for the toast itself.
    pub button: String,
    /// The item picked in the toast's drop-down, by id.
    pub choice: Option<String>,
}

fn show(app: &AppHandle, alert: &Alert, sound: bool) -> Result<(), String> {
    show_tagged(app, alert, sound, None)
}

/// Shows an alert; one with a `tag` replaces the last with it, and can be taken down.
fn show_tagged(app: &AppHandle, alert: &Alert, sound: bool, tag: Option<&str>) -> Result<(), String> {
    let message = alert.message;
    let code = alert.code.clone();
    let clicked = app.clone();
    show_xml_tagged(app, &toast_xml(alert, sound), None, tag, move |click| {
        if let (COPY_CODE, Some(id), Some(code)) = (click.button.as_str(), message, &code) {
            copy_code(&clicked, id, code);
            return;
        }
        background::show(&clicked);
        if let Some(id) = message {
            let _ = clicked.emit("open-message", id);
        }
    })
}

/// A toast's Copy button: the code goes on the clipboard, and its email is
/// marked as Settings → Notifications says.
fn copy_code(app: &AppHandle, id: i64, code: &str) {
    if let Err(e) = clipboard::set_text(code) {
        eprintln!("spark-rs: couldn't copy the sign-in code: {e}");
        return;
    }
    let after = app.state::<PrefsState>().get().after_code_copy;
    let mailbox = app.state::<Mailbox>();
    let marked = mark_copied(&mut mailbox.live.engine.store(), id, after, now_ms());
    match marked {
        Ok(Some(account)) => {
            let _ = app.emit("mailbox-changed", ());
            mailbox.live.nudge(account);
        }
        Ok(None) => {}
        Err(e) => eprintln!("spark-rs: couldn't mark the email whose code was copied: {e}"),
    }
}

/// Marks email `id`'s conversation read or Done once its code is copied,
/// as the Inbox's own buttons would: Done takes the emails kept where it
/// is. Returns its account if anything changed.
fn mark_copied(
    store: &mut Store,
    id: i64,
    after: AfterCodeCopy,
    now: i64,
) -> spark_store::Result<Option<i64>> {
    let Some(copied) = store.message(id)? else {
        return Ok(None);
    };
    let mut ids = store.conversation_before(id, 100)?;
    ids.push(id);
    let mut changed = false;
    for other in ids {
        let Some(m) = store.message(other)? else {
            continue;
        };
        let patch = match after {
            AfterCodeCopy::Keep => continue,
            AfterCodeCopy::Read if m.unread => Patch {
                unread: Some(false),
                ..Patch::default()
            },
            AfterCodeCopy::Done
                if copied.state == MessageState::Inbox && m.state == copied.state =>
            {
                Patch {
                    state: Some(MessageState::Done),
                    ..Patch::default()
                }
            }
            _ => continue,
        };
        changed |= store.update(other, &patch, now)?.is_some();
    }
    Ok(changed.then_some(copied.account_id))
}

/// Shows a toast written in Windows' XML schema; `on_click` runs for a
/// click on it or on one of its foreground buttons. `expires` (Unix ms)
/// takes it out of the notification center then.
pub fn show_xml(
    app: &AppHandle,
    xml: &str,
    expires: Option<i64>,
    on_click: impl Fn(Click) + Send + 'static,
) -> Result<(), String> {
    show_xml_tagged(app, xml, expires, None, on_click)
}

fn show_xml_tagged(
    app: &AppHandle,
    xml: &str,
    expires: Option<i64>,
    tag: Option<&str>,
    on_click: impl Fn(Click) + Send + 'static,
) -> Result<(), String> {
    #[cfg(windows)]
    return toast::show(app, xml, expires, tag, on_click);
    #[cfg(not(windows))]
    {
        let _ = (app, xml, expires, tag, on_click);
        Err("Notifications are only available on Windows for now.".into())
    }
}

/// Text on the Windows clipboard, for a toast's Copy button: the window
/// may be hidden, so the web view's clipboard can't do it.
#[cfg(windows)]
mod clipboard {
    use std::{ptr, thread, time::Duration};

    use windows::Win32::Foundation::{GlobalFree, HANDLE};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};

    /// The `CF_UNICODETEXT` format: UTF-16 ending in a zero.
    const UNICODE_TEXT: u32 = 13;

    pub fn set_text(text: &str) -> Result<(), String> {
        let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
        // Another app may have it open for a moment.
        let mut tries = 0;
        while unsafe { OpenClipboard(None) }.is_err() {
            tries += 1;
            if tries == 10 {
                return Err("another app is using the clipboard".into());
            }
            thread::sleep(Duration::from_millis(25));
        }
        let set = unsafe {
            EmptyClipboard().and_then(|()| {
                let memory = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2)?;
                let target = GlobalLock(memory).cast::<u16>();
                if target.is_null() {
                    let _ = GlobalFree(Some(memory));
                    return Err(windows::core::Error::from_thread());
                }
                ptr::copy_nonoverlapping(wide.as_ptr(), target, wide.len());
                let _ = GlobalUnlock(memory);
                // The clipboard owns the memory once it takes it.
                SetClipboardData(UNICODE_TEXT, Some(HANDLE(memory.0)))
                    .map(drop)
                    .inspect_err(|_| {
                        let _ = GlobalFree(Some(memory));
                    })
            })
        };
        let _ = unsafe { CloseClipboard() };
        set.map_err(|e| e.to_string())
    }
}

#[cfg(not(windows))]
mod clipboard {
    pub fn set_text(_text: &str) -> Result<(), String> {
        Err("Copying from a notification is only available on Windows.".into())
    }
}

#[cfg(windows)]
mod toast {
    use std::fs;
    use std::sync::Mutex;

    use tauri::{AppHandle, Manager};
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::Foundation::{DateTime, IReference, PropertyValue, TypedEventHandler};
    use windows::UI::Notifications::{
        NotificationSetting, ToastActivatedEventArgs, ToastNotification, ToastNotificationManager,
    };
    use windows::core::{HSTRING, IInspectable, Interface};

    use super::Click;

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

    /// 100 ns ticks between 1601-01-01 (Windows' epoch) and 1970-01-01.
    const UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;

    /// Toasts with a tag are in this group, so they can be found again.
    const GROUP: &str = "spark-rs";

    pub fn show(
        app: &AppHandle,
        xml: &str,
        expires: Option<i64>,
        tag: Option<&str>,
        on_click: impl Fn(Click) + Send + 'static,
    ) -> Result<(), String> {
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
        if let Some(at) = expires {
            let time = DateTime {
                UniversalTime: at * 10_000 + UNIX_EPOCH_TICKS,
            };
            let expiry = PropertyValue::CreateDateTime(time)
                .and_then(|v| v.cast::<IReference<DateTime>>())
                .map_err(|e| e.to_string())?;
            toast
                .SetExpirationTime(&expiry)
                .map_err(|e| e.to_string())?;
        }
        if let Some(tag) = tag {
            toast
                .SetTag(&HSTRING::from(tag))
                .and_then(|()| toast.SetGroup(&HSTRING::from(GROUP)))
                .map_err(|e| e.to_string())?;
        }
        toast
            .Activated(&TypedEventHandler::<ToastNotification, IInspectable>::new(
                move |_, args| {
                    on_click(click(args.as_ref()));
                    Ok(())
                },
            ))
            .map_err(|e| e.to_string())?;
        notifier.Show(&toast).map_err(|e| e.to_string())?;

        let mut shown = SHOWN.lock().unwrap();
        if shown.len() == KEEP {
            shown.remove(0);
        }
        shown.push(toast);
        Ok(())
    }

    /// The button and drop-down choice behind an activation.
    fn click(args: Option<&IInspectable>) -> Click {
        let Some(args) = args.and_then(|a| a.cast::<ToastActivatedEventArgs>().ok()) else {
            return Click::default();
        };
        let button = args
            .Arguments()
            .map(|a| a.to_string_lossy())
            .unwrap_or_default();
        let choice = args.UserInput().ok().and_then(|input| {
            let first = input.First().ok()?;
            let pair = first.Current().ok()?;
            let value = pair.Value().ok()?.cast::<IReference<HSTRING>>().ok()?;
            Some(value.Value().ok()?.to_string_lossy())
        });
        Click { button, choice }
    }

    /// Takes the toast with `tag` out of the notification center.
    pub fn remove(app: &AppHandle, tag: &str) -> Result<(), String> {
        ToastNotificationManager::History()
            .and_then(|history| {
                history.RemoveGroupedTagWithId(
                    &HSTRING::from(tag),
                    &HSTRING::from(GROUP),
                    &HSTRING::from(&app.config().identifier),
                )
            })
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use spark_store::{AccountKind, Added, Incoming, NewAccount};

    use super::*;

    const NOW: i64 = 1_790_000_000_000;

    /// Mail with a sign-in code, a minute old, from a sender `notify` leaves out.
    fn code_mail(id: i64) -> Message {
        Message {
            category: Category::Notifications,
            screening: true,
            date: NOW - 60_000,
            sign_in_code: Some(format!("48291{id}")),
            ..message(id, "Fernway", "Your sign-in code")
        }
    }

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
            reply_to: Vec::new(),
            subject: subject.into(),
            snippet: format!("About {subject}"),
            date: 0,
            attachment: None,
            folders: Vec::new(),
            folder: None,
            to: Vec::new(),
            conversation: id,
            returned_at: None,
            muted: false,
            reminder: None,
            invite_kind: None,
            auto_label: None,
            auto_draft: None,
            content_rev: 0,
            unsubscribe: None,
            sign_in_code: None,
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
            alerts(&arrived, NotifyFor::People, None, None),
            [Alert {
                title: "Ana".into(),
                lines: vec!["Lunch?".into(), "About Lunch?".into()],
                attribution: None,
                message: Some(1),
                code: None,
            }]
        );
        assert_eq!(
            alerts(&arrived, NotifyFor::All, None, Some("Work")).len(),
            2
        );
        assert!(alerts(&arrived, NotifyFor::Off, None, None).is_empty());
    }

    #[test]
    fn priority_notifies_only_about_priority_mail() {
        let arrived = [
            message(1, "Ana", "Lunch?"),
            Message {
                priority: true,
                category: Category::Notifications,
                ..message(2, "Boss", "Budget")
            },
        ];
        let shown = alerts(&arrived, NotifyFor::Priority, None, None);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].message, Some(2));
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
        assert!(alerts(&arrived, NotifyFor::All, None, None).is_empty());
    }

    #[test]
    fn due_reminders_say_who_hasnt_replied() {
        let reminder = Reminder {
            id: 1,
            account_id: 1,
            subject: "Lease".into(),
            to: "Ana <ana@x.com>".into(),
            remind_at: 0,
            if_no_reply: true,
            due: true,
            message: Some(9),
        };
        let alerts = reminder_alerts(std::slice::from_ref(&reminder));
        assert_eq!(alerts[0].title, "No reply yet");
        assert_eq!(alerts[0].lines, ["Lease", "To Ana <ana@x.com>"]);
        assert_eq!(alerts[0].message, Some(9));
        assert_eq!(reminder_alerts(&vec![reminder; 5]).len(), 1);
    }

    #[test]
    fn a_returning_conversation_is_one_toast_and_muted_mail_none() {
        let thread = |id, date| Message {
            conversation: 1,
            date,
            ..message(id, "Ana", "Plan")
        };
        let returned = returned_alerts(&[thread(1, 10), thread(2, 30), thread(3, 20)]);
        assert_eq!(returned.len(), 1);
        assert_eq!(returned[0].message, Some(2));
        let muted = Message {
            muted: true,
            ..message(4, "Bob", "Quiet")
        };
        assert!(alerts(&[muted], NotifyFor::All, None, None).is_empty());
    }

    #[test]
    fn many_at_once_become_one_toast() {
        let arrived: Vec<Message> = ["Ana", "Bob", "Ana", "Cy", "Dee"]
            .into_iter()
            .enumerate()
            .map(|(i, from)| message(i as i64, from, "Hello"))
            .collect();
        assert_eq!(
            alerts(&arrived, NotifyFor::People, None, Some("Work")),
            [Alert {
                title: "5 new emails".into(),
                lines: vec!["From Ana, Bob and 2 others".into()],
                attribution: Some("Work".into()),
                message: None,
                code: None,
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
        let alert = &alerts(&arrived, NotifyFor::People, None, None)[0];
        assert_eq!(alert.title, "ana@x.com");
        assert_eq!(alert.lines, ["(no subject)"]);
    }

    #[test]
    fn the_focus_reminder_counts_new_mail() {
        assert_eq!(focus_alert(0).lines, vec!["There are no new emails"]);
        assert_eq!(focus_alert(1).lines, vec!["You've got 1 new email"]);
        let alert = focus_alert(12);
        assert_eq!(alert.title, "It's time for Email Focus!");
        assert_eq!(alert.lines, vec!["You've got 12 new emails"]);
        assert_eq!(alert.message, None);
    }

    #[test]
    fn fresh_sign_in_codes_notify_from_any_sender_with_a_copy_button() {
        let shown = alerts(&[code_mail(1)], NotifyFor::People, Some(NOW), None);
        assert_eq!(shown.len(), 1);
        assert_eq!(
            (shown[0].message, shown[0].code.as_deref()),
            (Some(1), Some("482911"))
        );
        // Not with the setting off, nor once it's stale, muted, blocked, or
        // with notifications off.
        assert!(alerts(&[code_mail(1)], NotifyFor::People, None, None).is_empty());
        let stale = Message {
            date: NOW - CODE_FRESH_MS - 1,
            ..code_mail(1)
        };
        assert!(alerts(&[stale], NotifyFor::People, Some(NOW), None).is_empty());
        let muted = Message {
            muted: true,
            ..code_mail(1)
        };
        let blocked = Message {
            state: MessageState::Blocked,
            ..code_mail(2)
        };
        assert!(alerts(&[muted, blocked], NotifyFor::All, Some(NOW), None).is_empty());
        assert!(alerts(&[code_mail(1)], NotifyFor::Off, Some(NOW), None).is_empty());
        // Mail that notifies anyway still gets its button.
        let from_people = Message {
            category: Category::People,
            screening: false,
            date: 0,
            ..code_mail(3)
        };
        assert_eq!(
            alerts(&[from_people], NotifyFor::People, None, None)[0]
                .code
                .as_deref(),
            Some("482913")
        );
    }

    #[test]
    fn a_code_keeps_its_own_toast_among_many_and_shows_last() {
        let mut arrived: Vec<Message> = ["Ana", "Bob", "Cy", "Dee"]
            .into_iter()
            .enumerate()
            .map(|(i, from)| message(i as i64 + 10, from, "Hello"))
            .collect();
        arrived.insert(1, code_mail(1));
        let shown = alerts(&arrived, NotifyFor::People, Some(NOW), None);
        assert_eq!(shown.len(), 2);
        assert_eq!(shown[0].title, "4 new emails");
        assert_eq!(shown[1].code.as_deref(), Some("482911"));
    }

    #[test]
    fn a_code_toast_has_a_copy_button() {
        let alert = &alerts(&[code_mail(1)], NotifyFor::People, Some(NOW), None)[0];
        assert!(toast_xml(alert, false).contains(
            r#"</visual><actions><action activationType="foreground" arguments="copy-code" content="Copy 482911"/></actions><audio"#
        ));
        let plain = &alerts(
            &[message(2, "Ana", "Lunch?")],
            NotifyFor::People,
            None,
            None,
        )[0];
        assert!(!toast_xml(plain, false).contains("<actions>"));
    }

    #[test]
    fn copying_a_code_marks_its_conversation_as_asked() {
        let mut store = Store::open_in_memory().unwrap();
        let account = store
            .add_account(&NewAccount {
                kind: AccountKind::Imap,
                email: "me@x.com",
                name: "Me",
                label: "Me",
                config: "{}",
            })
            .unwrap()
            .id;
        let mut add = |uid: u32, reply_to: Option<&str>| {
            let incoming = Incoming {
                account_id: account,
                mailbox: Some("INBOX".into()),
                uid: Some(uid),
                message_id: Some(format!("{uid}@fernway.example")),
                in_reply_to: reply_to.map(str::to_owned),
                from_name: "Fernway".into(),
                from_address: "no-reply@fernway.example".into(),
                subject: "Your sign-in code".into(),
                date: NOW - 1_000 + i64::from(uid),
                unread: true,
                sign_in_code: Some("482913".into()),
                ..Default::default()
            };
            match store.add_incoming(&incoming, false).unwrap() {
                Added::New(id) => id,
                other => panic!("not stored: {other:?}"),
            }
        };
        let older = add(1, None);
        let newer = add(2, Some("1@fernway.example"));
        drop(add);
        let read = |store: &Store, id| store.message(id).unwrap().unwrap();

        assert_eq!(
            mark_copied(&mut store, newer, AfterCodeCopy::Keep, NOW).unwrap(),
            None
        );
        assert!(read(&store, older).unread && read(&store, newer).unread);

        assert_eq!(
            mark_copied(&mut store, newer, AfterCodeCopy::Read, NOW).unwrap(),
            Some(account)
        );
        assert!(!read(&store, older).unread && !read(&store, newer).unread);
        // Nothing left to mark.
        assert_eq!(
            mark_copied(&mut store, newer, AfterCodeCopy::Read, NOW).unwrap(),
            None
        );

        assert_eq!(
            mark_copied(&mut store, newer, AfterCodeCopy::Done, NOW).unwrap(),
            Some(account)
        );
        assert_eq!(read(&store, older).state, MessageState::Done);
        assert_eq!(read(&store, newer).state, MessageState::Done);
        assert_eq!(
            mark_copied(&mut store, 999, AfterCodeCopy::Done, NOW).unwrap(),
            None
        );
    }

    #[test]
    fn toast_xml_escapes_text_and_picks_the_sound() {
        let alert = Alert {
            title: "Ana <ana@x.com>".into(),
            lines: vec!["Q&A\u{b}\ttomorrow".into()],
            attribution: Some("Work".into()),
            message: Some(1),
            code: None,
        };
        assert_eq!(
            toast_xml(&alert, true),
            r#"<toast><visual><binding template="ToastGeneric"><text>Ana &lt;ana@x.com&gt;</text><text>Q&amp;A tomorrow</text><text placement="attribution">Work</text></binding></visual><audio src="ms-winsoundevent:Notification.Mail"/></toast>"#
        );
        assert!(toast_xml(&alert, false).ends_with(r#"<audio silent="true"/></toast>"#));
    }
}
