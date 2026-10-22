//! What the UI talks to: real accounts once one is added, the demo mailbox
//! until then. Types here are the UI's shapes (`src/types.ts` mirrors them).

use serde::Serialize;
use spark_mail::draft::{self, Draft};
use spark_mail::engine::gmail_servers;
use spark_mail::imap::Server;
use spark_store::{Account, AccountKind, Category, MessageState, Patch, now_ms};
use tauri::{AppHandle, Emitter, State};

use crate::demo::{self, DemoMailbox};
use crate::live::Live;

pub struct Mailbox {
    pub demo: DemoMailbox,
    pub live: Live,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: i64,
    pub account: i64,
    pub category: Category,
    pub state: MessageState,
    /// Unix ms when a snoozed message comes back.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snoozed_until: Option<i64>,
    pub unread: bool,
    pub pinned: bool,
    pub priority: bool,
    /// From a sender not yet accepted through the Gatekeeper.
    pub screening: bool,
    pub from: String,
    pub address: String,
    /// When the message arrived, in Unix milliseconds.
    pub timestamp: i64,
    pub subject: String,
    pub snippet: String,
    /// Paragraphs. Only demo messages carry them in the list; real ones load
    /// through `get_message_content`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachment: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountView {
    pub id: i64,
    pub kind: AccountKind,
    pub email: String,
    pub name: String,
    pub label: String,
    pub color: String,
    pub provider: &'static str,
    pub demo: bool,
    pub sync: SyncView,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncStatus {
    #[default]
    Idle,
    Syncing,
    Error,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncView {
    pub status: SyncStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_sync: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageContent {
    text: Vec<String>,
    /// The email's own HTML, untrusted: the UI renders it in a sandboxed frame.
    html: Option<String>,
    to: Vec<String>,
    cc: Vec<String>,
    attachments: Vec<String>,
}

impl Mailbox {
    fn live(&self) -> bool {
        self.live.has_accounts()
    }

    /// A message with its text, for +AI Summary.
    pub fn with_body(&self, id: i64) -> Option<Message> {
        if !self.live() {
            return self.demo.get(id);
        }
        let store = self.live.engine.store();
        let mut message = from_store(store.message(id).ok()??);
        message.body = Some(paragraphs(&store.content(id).ok()??.text));
        Some(message)
    }

    fn account_view(&self, account: Account) -> AccountView {
        AccountView {
            id: account.id,
            kind: account.kind,
            provider: match account.kind {
                AccountKind::Gmail => "Gmail",
                AccountKind::Microsoft => "Microsoft 365",
                AccountKind::Imap => "IMAP",
            },
            sync: self.live.status(account.id),
            email: account.email,
            name: account.name,
            label: account.label,
            color: account.color,
            demo: false,
        }
    }

    fn added(&self, app: &AppHandle, account: Account) -> AccountView {
        self.live.start(app, account.id);
        let _ = app.emit("accounts-changed", ());
        let _ = app.emit("mailbox-changed", ());
        self.account_view(account)
    }
}

fn from_store(m: spark_store::Message) -> Message {
    Message {
        id: m.id,
        account: m.account_id,
        category: m.category,
        state: m.state,
        snoozed_until: m.snoozed_until,
        unread: m.unread,
        pinned: m.pinned,
        priority: m.priority,
        screening: m.screening,
        from: m.from_name,
        address: m.from_address,
        timestamp: m.date,
        subject: m.subject,
        snippet: m.snippet,
        body: None,
        attachment: m.attachment,
    }
}

/// Blank lines separate paragraphs; lines within one keep their breaks.
fn paragraphs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in text.lines().map(str::trim_end) {
        if line.trim().is_empty() {
            if !current.is_empty() {
                out.push(current.join("\n"));
                current.clear();
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        out.push(current.join("\n"));
    }
    out
}

fn text(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[tauri::command]
pub fn list_accounts(mailbox: State<'_, Mailbox>) -> Result<Vec<AccountView>, String> {
    if !mailbox.live() {
        return Ok(demo::accounts());
    }
    let accounts = mailbox.live.engine.store().accounts().map_err(text)?;
    Ok(accounts
        .into_iter()
        .map(|a| mailbox.account_view(a))
        .collect())
}

#[tauri::command]
pub fn list_messages(mailbox: State<'_, Mailbox>) -> Result<Vec<Message>, String> {
    if !mailbox.live() {
        return Ok(mailbox.demo.list());
    }
    let messages = mailbox.live.engine.store().messages().map_err(text)?;
    Ok(messages.into_iter().map(from_store).collect())
}

#[tauri::command]
pub fn get_message_content(mailbox: State<'_, Mailbox>, id: i64) -> Result<MessageContent, String> {
    if !mailbox.live() {
        let m = mailbox
            .demo
            .get(id)
            .ok_or_else(|| format!("no message with id {id}"))?;
        return Ok(MessageContent {
            text: m.body.unwrap_or_default(),
            html: None,
            to: vec!["You".into()],
            cc: Vec::new(),
            attachments: m.attachment.into_iter().collect(),
        });
    }
    let content = mailbox
        .live
        .engine
        .store()
        .content(id)
        .map_err(text)?
        .ok_or_else(|| format!("no message with id {id}"))?;
    Ok(MessageContent {
        text: paragraphs(&content.text),
        html: content.html,
        to: content.to,
        cc: content.cc,
        attachments: content.attachments,
    })
}

#[tauri::command]
pub fn update_message(
    mailbox: State<'_, Mailbox>,
    id: i64,
    patch: Patch,
) -> Result<Message, String> {
    if !mailbox.live() {
        return mailbox.demo.update(id, &patch);
    }
    let updated = mailbox
        .live
        .engine
        .store()
        .update(id, &patch, now_ms())
        .map_err(text)?;
    updated
        .map(from_store)
        .ok_or_else(|| format!("no message with id {id}"))
}

#[tauri::command]
pub async fn add_gmail_account(
    app: AppHandle,
    mailbox: State<'_, Mailbox>,
    email: String,
    app_password: String,
    label: String,
) -> Result<AccountView, String> {
    let (imap, smtp) = gmail_servers();
    let account = mailbox
        .live
        .engine
        .add_password_account(
            AccountKind::Gmail,
            &email,
            &app_password,
            &label,
            imap,
            smtp,
        )
        .await
        .map_err(text)?;
    Ok(mailbox.added(&app, account))
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn add_imap_account(
    app: AppHandle,
    mailbox: State<'_, Mailbox>,
    email: String,
    password: String,
    label: String,
    imap_host: String,
    imap_port: u16,
    smtp_host: String,
    smtp_port: u16,
) -> Result<AccountView, String> {
    if imap_host.trim().is_empty() || smtp_host.trim().is_empty() {
        return Err("Enter the IMAP and SMTP servers.".into());
    }
    let account = mailbox
        .live
        .engine
        .add_password_account(
            AccountKind::Imap,
            &email,
            &password,
            &label,
            Server::new(&imap_host, imap_port),
            Server::new(&smtp_host, smtp_port),
        )
        .await
        .map_err(text)?;
    Ok(mailbox.added(&app, account))
}

/// Opens Microsoft sign-in in the browser; resolves when it's done.
#[tauri::command]
pub async fn add_microsoft_account(
    app: AppHandle,
    mailbox: State<'_, Mailbox>,
    client_id: String,
    tenant: String,
    label: String,
) -> Result<AccountView, String> {
    let cancelled = mailbox.live.sign_in_cancel();
    let sign_in = mailbox
        .live
        .engine
        .add_microsoft_account(&client_id, &tenant, &label, |url| {
            open::that_detached(url).map_err(|e| e.to_string())
        });
    let account = tokio::select! {
        result = sign_in => result.map_err(text)?,
        _ = cancelled => return Err("Sign-in cancelled.".into()),
    };
    Ok(mailbox.added(&app, account))
}

#[tauri::command]
pub fn cancel_sign_in(mailbox: State<'_, Mailbox>) {
    mailbox.live.cancel_sign_in();
}

#[tauri::command]
pub fn remove_account(app: AppHandle, mailbox: State<'_, Mailbox>, id: i64) -> Result<(), String> {
    mailbox.live.stop(id);
    mailbox.live.engine.remove_account(id).map_err(text)?;
    let _ = app.emit("accounts-changed", ());
    let _ = app.emit("mailbox-changed", ());
    Ok(())
}

#[tauri::command]
pub fn rename_account(
    app: AppHandle,
    mailbox: State<'_, Mailbox>,
    id: i64,
    label: String,
) -> Result<AccountView, String> {
    let label = label.trim();
    if label.is_empty() {
        return Err("Enter a name for the account.".into());
    }
    let store = mailbox.live.engine.store();
    store.rename_account(id, label).map_err(text)?;
    let account = store
        .account(id)
        .map_err(text)?
        .ok_or_else(|| "That account was removed.".to_owned())?;
    drop(store);
    let _ = app.emit("accounts-changed", ());
    Ok(mailbox.account_view(account))
}

#[tauri::command]
pub fn sync_now(mailbox: State<'_, Mailbox>) {
    mailbox.live.sync_now();
}

/// Queues a draft to go out at `draft.sendAt`; returns its outbox id.
#[tauri::command]
pub fn send_message(mailbox: State<'_, Mailbox>, draft: Draft) -> Result<i64, String> {
    if !mailbox.live() {
        // The demo pretends; ids only need to be unique.
        return Ok(now_ms());
    }
    // Catch typos now rather than after the undo window.
    for field in [&draft.to, &draft.cc, &draft.bcc] {
        draft::recipients(field).map_err(text)?;
    }
    let json = serde_json::to_string(&draft).map_err(text)?;
    let id = mailbox
        .live
        .engine
        .store()
        .queue_send(draft.account, &json, draft.send_at)
        .map_err(text)?;
    if draft.send_at <= now_ms() {
        mailbox.live.nudge(draft.account);
    }
    Ok(id)
}

/// True if the send was stopped in time.
#[tauri::command]
pub fn cancel_send(mailbox: State<'_, Mailbox>, id: i64) -> Result<bool, String> {
    if !mailbox.live() {
        return Ok(true);
    }
    mailbox.live.engine.store().cancel_send(id).map_err(text)
}

/// Opens a link from an email in the system browser or mail handler.
#[tauri::command]
pub fn open_external(url: String) -> Result<(), String> {
    let parsed = url::Url::parse(&url).map_err(|_| "That link isn't valid.".to_owned())?;
    if !matches!(parsed.scheme(), "http" | "https" | "mailto") {
        return Err(format!("spark-rs doesn't open {} links.", parsed.scheme()));
    }
    open::that_detached(parsed.as_str()).map_err(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paragraphs_split_on_blank_lines() {
        assert_eq!(
            paragraphs("Hi Ana,\n\nLine one\nline two  \n \n\n\nBye\n"),
            vec!["Hi Ana,", "Line one\nline two", "Bye"]
        );
        assert!(paragraphs("").is_empty());
    }

    #[test]
    fn messages_serialize_in_the_ui_shape() {
        let m = from_store(spark_store::Message {
            id: 3,
            account_id: 7,
            category: Category::Newsletters,
            state: MessageState::Snoozed,
            snoozed_until: Some(5),
            unread: true,
            pinned: false,
            priority: false,
            screening: false,
            from_name: "Ana".into(),
            from_address: "ana@x.com".into(),
            subject: "Hi".into(),
            snippet: "…".into(),
            date: 9,
            attachment: None,
        });
        let json = serde_json::to_value(&m).unwrap();
        assert_eq!(json["account"], 7);
        assert_eq!(json["from"], "Ana");
        assert_eq!(json["address"], "ana@x.com");
        assert_eq!(json["timestamp"], 9);
        assert_eq!(json["snoozedUntil"], 5);
        assert_eq!(json["category"], "newsletters");
        assert_eq!(json["state"], "snoozed");
        assert!(json.get("body").is_none() && json.get("attachment").is_none());
    }
}
