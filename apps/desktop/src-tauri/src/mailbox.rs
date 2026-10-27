//! What the UI talks to: real accounts once one is added, the demo mailbox
//! until then. Types here are the UI's shapes (`src/types.ts` mirrors them).

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Serialize;
use spark_mail::attachment::{AttachmentData, InlineImage, cid_refs, mime_for};
use spark_mail::draft::{self, Draft, OutgoingAttachment};
use spark_mail::engine::{folder_has_older, gmail_servers, inbox_has_older};
use spark_mail::imap::Server;
use spark_mail::invite::{Answer, Card, Invite};
use spark_mail::signature;
use spark_mail::unsubscribe::Unsubscribed;
use spark_mail::{Error, Service, Untrusted};
use spark_store::{
    Account, AccountKind, Category, Change, Contact, DueReminder, Folder, FolderRole,
    GatekeeperMode, MessageState, OutboxFile, Patch, PriorityUndo, SenderStatus, now_ms,
};
use tauri::{AppHandle, Emitter, Manager, State, Window};

use crate::demo::{self, DemoMailbox};
use crate::files::{self, LocalFile};
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
    /// Reply-To display entries, when replies should use another address.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reply_to: Vec<String>,
    /// Who it's to: "Name <address>" or the bare address. Sent and Drafts
    /// list them instead of the sender.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub to: Vec<String>,
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
    /// Ids of the folders that hold it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<i64>,
    /// While Filed: the folder it's kept in (its home).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<i64>,
    /// Messages of one conversation share it; without one, a message is a
    /// conversation of its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation: Option<i64>,
    /// When it came back from Snooze: the Inbox puts it there, at the top.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub returned_at: Option<i64>,
    /// Its conversation is muted: new mail in it doesn't notify.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub muted: bool,
    /// A reminder that came due on it: it's in the Inbox until Done.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reminder: Option<DueReminder>,
    /// The calendar invite it carries, by kind ("invitation", "reply", …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invite_kind: Option<String>,
}

/// Mail scheduled with Send Later, for the Scheduled list.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledView {
    /// Its outbox id.
    pub id: i64,
    pub account: i64,
    pub to: String,
    pub subject: String,
    pub snippet: String,
    pub send_at: i64,
}

/// A reminder, for the Reminders list.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReminderView {
    pub id: i64,
    pub account: i64,
    pub to: String,
    pub subject: String,
    pub remind_at: i64,
    pub if_no_reply: bool,
    /// It came due and waits in the Inbox.
    pub due: bool,
    /// The stored email it came back as.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<i64>,
}

/// A server folder or label, for the sidebar and folder picker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderView {
    pub id: i64,
    pub account: i64,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<i64>,
    /// What it's for; absent for the user's own folders.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<FolderRole>,
    /// False for folders that only hold other folders.
    pub selectable: bool,
    /// Its messages have been synced at least once.
    pub synced: bool,
    /// It has messages older than the ones synced, which `load_older`
    /// reaches back for.
    pub has_older: bool,
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
    /// The server has inbox mail older than what's synced, which
    /// `load_older` reaches back for.
    pub has_older: bool,
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
    /// The sync failed on a server certificate the user may trust.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate: Option<Untrusted>,
}

/// A failed command, for the UI: what went wrong, and for a server
/// certificate the system doesn't trust, which one, so the user can choose
/// to trust it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Failure {
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    certificate: Option<Untrusted>,
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self {
            message,
            certificate: None,
        }
    }
}

impl From<Error> for Failure {
    fn from(e: Error) -> Self {
        Self {
            certificate: certificate_of(&e),
            message: e.to_string(),
        }
    }
}

/// The certificate a failure was about, if it was one the user may trust.
pub(crate) fn certificate_of(e: &Error) -> Option<Untrusted> {
    match e {
        Error::Certificate(untrusted) => Some((**untrusted).clone()),
        _ => None,
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageContent {
    text: Vec<String>,
    /// The email's own HTML, untrusted: the UI renders it in a sandboxed frame.
    html: Option<String>,
    to: Vec<String>,
    cc: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    reply_to: Vec<String>,
    attachments: Vec<AttachmentView>,
    /// The calendar invite it carries, shown as a card in place of the body.
    #[serde(skip_serializing_if = "Option::is_none")]
    invite: Option<Card>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentView {
    name: String,
    /// e.g. "application/pdf"; empty when unknown.
    mime: String,
    /// Bytes; 0 when unknown.
    size: u64,
}

const DEMO_FILES: &str = "This is demo mail: its attachments are only names.";
const DEMO_FOLDERS: &str = "These are demo folders: add an account to manage your own.";

impl Mailbox {
    pub(crate) fn live(&self) -> bool {
        self.live.has_accounts()
    }

    /// A message with its text, for the +AI prompts.
    pub fn with_body(&self, id: i64) -> Option<Message> {
        if !self.live() {
            return self.demo.get(id);
        }
        let store = self.live.engine.store();
        let mut message = from_store(store.message(id).ok()??);
        message.body = Some(paragraphs(&store.content(id).ok()??.text));
        Some(message)
    }

    /// The user's own name on an account, to sign the replies AI writes. The
    /// demo accounts have none.
    pub fn user_name(&self, account: i64) -> Option<String> {
        if !self.live() {
            return None;
        }
        let account = self.live.engine.store().account(account).ok()??;
        let name = account.name.trim();
        (!name.is_empty()).then(|| name.to_owned())
    }

    /// The address account `id` sends from.
    pub fn account_email(&self, id: i64) -> Option<String> {
        if !self.live() {
            return demo::accounts()
                .into_iter()
                .find(|a| a.id == id)
                .map(|a| a.email);
        }
        Some(self.live.engine.store().account(id).ok()??.email)
    }

    /// Every account's address, in order.
    pub fn addresses(&self) -> Vec<String> {
        if !self.live() {
            return demo::accounts().into_iter().map(|a| a.email).collect();
        }
        let accounts = self.live.engine.store().accounts().unwrap_or_default();
        accounts.into_iter().map(|a| a.email).collect()
    }

    pub(crate) fn account_view(&self, account: Account) -> AccountView {
        AccountView {
            id: account.id,
            kind: account.kind,
            provider: match account.kind {
                AccountKind::Gmail => "Gmail",
                AccountKind::Microsoft => "Microsoft 365",
                AccountKind::Imap => "IMAP",
            },
            sync: self.live.status(account.id),
            has_older: inbox_has_older(&account),
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
        reply_to: m.reply_to,
        to: m.to,
        timestamp: m.date,
        subject: m.subject,
        snippet: m.snippet,
        body: None,
        attachment: m.attachment,
        folders: m.folders,
        folder: m.folder,
        conversation: Some(m.conversation),
        returned_at: m.returned_at,
        muted: m.muted,
        reminder: m.reminder,
        invite_kind: m.invite_kind,
    }
}

fn folder_view(f: Folder) -> FolderView {
    FolderView {
        has_older: folder_has_older(&f),
        id: f.id,
        account: f.account_id,
        name: f.name,
        parent: f.parent_id,
        role: f.role,
        selectable: f.selectable,
        synced: f.synced_at.is_some(),
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

pub(crate) fn text(e: impl std::fmt::Display) -> String {
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

/// The stored messages whose words start with those of `query`, newest
/// first. None in the demo, where the UI matches the messages it has.
#[tauri::command]
pub fn search_messages(
    mailbox: State<'_, Mailbox>,
    query: String,
) -> Result<Option<Vec<i64>>, String> {
    if !mailbox.live() {
        return Ok(None);
    }
    let found = mailbox.live.engine.store().search(&query).map_err(text)?;
    Ok(Some(found.into_iter().map(|m| m.id).collect()))
}

/// How many people a recipient field suggests at most.
const SUGGESTIONS: usize = 20;

/// The people to suggest for what's typed in a recipient field, best first.
#[tauri::command]
pub fn suggest_contacts(
    mailbox: State<'_, Mailbox>,
    query: String,
) -> Result<Vec<Contact>, String> {
    if !mailbox.live() {
        return Ok(mailbox.demo.suggest_contacts(&query, SUGGESTIONS));
    }
    let store = mailbox.live.engine.store();
    store
        .suggest_contacts(&query, now_ms(), SUGGESTIONS)
        .map_err(text)
}

/// Stops suggesting `address` (the ✕ on a suggestion).
#[tauri::command]
pub fn never_suggest_contact(mailbox: State<'_, Mailbox>, address: String) -> Result<(), String> {
    if !mailbox.live() {
        mailbox.demo.never_suggest(&address);
        return Ok(());
    }
    mailbox
        .live
        .engine
        .store()
        .never_suggest(&address)
        .map_err(text)
}

/// Puts a search to every account's server and stores the mail they find
/// that isn't stored yet, for `search_messages` to find; see
/// `Live::search_servers`. Returns how many messages were new, so the UI
/// knows to reload. 0 in the demo, and for a search a newer one cancelled,
/// after telling the UI to reload what it stored before it stopped.
#[tauri::command]
pub async fn search_servers(
    app: AppHandle,
    mailbox: State<'_, Mailbox>,
    query: String,
    account: Option<i64>,
) -> Result<usize, String> {
    if !mailbox.live() {
        return Ok(0);
    }
    match mailbox.live.search_servers(&query, account).await {
        Some(new) => Ok(new),
        None => {
            let _ = app.emit("mailbox-changed", ());
            Ok(0)
        }
    }
}

/// Every account's folders, accounts in order.
#[tauri::command]
pub fn list_folders(mailbox: State<'_, Mailbox>) -> Result<Vec<FolderView>, String> {
    if !mailbox.live() {
        return Ok(demo::folders());
    }
    let folders = mailbox.live.engine.store().folders().map_err(text)?;
    Ok(folders.into_iter().map(folder_view).collect())
}

/// The folders to keep in sync: those on the sidebar or open. New ones
/// sync at once.
#[tauri::command]
pub fn watch_folders(mailbox: State<'_, Mailbox>, ids: Vec<i64>) {
    mailbox.live.watch_folders(ids);
}

/// Syncs these folders now, e.g. the one just opened.
#[tauri::command]
pub fn sync_folders(mailbox: State<'_, Mailbox>, ids: Vec<i64>) {
    mailbox.live.sync_folders(ids);
}

/// Reaches 100 messages further back in these accounts' inboxes and these
/// folders, e.g. when a list is scrolled near its end. Resolves once their
/// accounts' rounds are over, even if one failed (that shows as a
/// `sync-status` error); the UI then reloads.
#[tauri::command]
pub async fn load_older(
    mailbox: State<'_, Mailbox>,
    accounts: Vec<i64>,
    folders: Vec<i64>,
) -> Result<(), String> {
    if !mailbox.live() {
        return Ok(());
    }
    for done in mailbox.live.load_older(accounts, folders) {
        // A worker that stopped counts as done.
        let _ = done.await;
    }
    Ok(())
}

/// Lists every account's folders again, e.g. when the folder picker opens.
#[tauri::command]
pub fn refresh_folders(mailbox: State<'_, Mailbox>) {
    mailbox.live.refresh_folders();
}

#[tauri::command]
pub async fn create_folder(
    app: AppHandle,
    mailbox: State<'_, Mailbox>,
    account: i64,
    parent: Option<i64>,
    name: String,
) -> Result<FolderView, String> {
    if !mailbox.live() {
        return Err(DEMO_FOLDERS.into());
    }
    let id = mailbox
        .live
        .engine
        .create_folder(account, parent, &name)
        .await
        .map_err(text)?;
    let _ = app.emit("folders-changed", ());
    let folder = mailbox.live.engine.store().folder(id).map_err(text)?;
    folder
        .map(folder_view)
        .ok_or_else(|| "The new folder is gone.".to_owned())
}

#[tauri::command]
pub async fn rename_folder(
    app: AppHandle,
    mailbox: State<'_, Mailbox>,
    id: i64,
    name: String,
) -> Result<(), String> {
    if !mailbox.live() {
        return Err(DEMO_FOLDERS.into());
    }
    mailbox
        .live
        .engine
        .rename_folder(id, &name)
        .await
        .map_err(text)?;
    let _ = app.emit("folders-changed", ());
    Ok(())
}

/// Deletes a folder, its subfolders and the mail in them on the server.
#[tauri::command]
pub async fn delete_folder(
    app: AppHandle,
    mailbox: State<'_, Mailbox>,
    id: i64,
) -> Result<(), String> {
    if !mailbox.live() {
        return Err(DEMO_FOLDERS.into());
    }
    mailbox.live.engine.delete_folder(id).await.map_err(text)?;
    let _ = app.emit("folders-changed", ());
    let _ = app.emit("mailbox-changed", ());
    Ok(())
}

/// Mail scheduled with Send Later, soonest first.
#[tauri::command]
pub fn list_scheduled(mailbox: State<'_, Mailbox>) -> Result<Vec<ScheduledView>, String> {
    if !mailbox.live() {
        return Ok(Vec::new());
    }
    let scheduled = mailbox
        .live
        .engine
        .store()
        .scheduled_sends()
        .map_err(text)?;
    Ok(scheduled
        .into_iter()
        .filter_map(|(id, account, json, send_at)| {
            let draft: Draft = serde_json::from_str(&json).ok()?;
            Some(ScheduledView {
                id,
                account,
                to: draft.to,
                subject: draft.subject,
                snippet: spark_mail::parse::snippet(&draft.body),
                send_at,
            })
        })
        .collect())
}

/// Sends a scheduled message at `at` instead, or now. False once it's gone out.
#[tauri::command]
pub fn reschedule_send(
    mailbox: State<'_, Mailbox>,
    id: i64,
    at: Option<i64>,
) -> Result<bool, String> {
    if !mailbox.live() {
        return Ok(false);
    }
    let moved = mailbox
        .live
        .engine
        .store()
        .reschedule_send(id, at, now_ms())
        .map_err(text)?;
    // Sent now: the account's worker sends it on its next round.
    if moved && at.is_none() {
        mailbox.live.sync_now();
    }
    Ok(moved)
}

/// Takes a scheduled message out of the queue into Drafts, exactly as it
/// was queued, to edit (`keep_schedule`: with its time) or to keep. Returns
/// the draft's id, or `None` once it's gone out.
#[tauri::command]
pub fn unschedule_send(
    app: AppHandle,
    mailbox: State<'_, Mailbox>,
    id: i64,
    keep_schedule: bool,
) -> Result<Option<i64>, String> {
    if !mailbox.live() {
        return Ok(None);
    }
    let draft = mailbox
        .live
        .engine
        .unschedule(id, keep_schedule)
        .map_err(text)?;
    let _ = app.emit("mailbox-changed", ());
    Ok(draft)
}

/// Reminders to come, and those waiting in the Inbox.
#[tauri::command]
pub fn list_reminders(mailbox: State<'_, Mailbox>) -> Result<Vec<ReminderView>, String> {
    if !mailbox.live() {
        return Ok(Vec::new());
    }
    let reminders = mailbox.live.engine.store().reminders().map_err(text)?;
    Ok(reminders
        .into_iter()
        .map(|r| ReminderView {
            id: r.id,
            account: r.account_id,
            to: r.to,
            subject: r.subject,
            remind_at: r.remind_at,
            if_no_reply: r.if_no_reply,
            due: r.due,
            message: r.message,
        })
        .collect())
}

/// Moves a reminder to `at`, or removes it.
#[tauri::command]
pub fn set_reminder(mailbox: State<'_, Mailbox>, id: i64, at: Option<i64>) -> Result<(), String> {
    if !mailbox.live() {
        return Ok(());
    }
    mailbox
        .live
        .engine
        .store()
        .set_reminder(id, at)
        .map_err(text)
}

/// Done on a reminder in the Inbox, or its undo.
#[tauri::command]
pub fn finish_reminder(mailbox: State<'_, Mailbox>, id: i64, done: bool) -> Result<(), String> {
    if !mailbox.live() {
        return Ok(());
    }
    mailbox
        .live
        .engine
        .store()
        .finish_reminder(id, done)
        .map_err(text)
}

/// Files a sender's mail under `category` in every account, now and later
/// (Spark's Change category); `None` goes back to what the headers say.
/// Returns the sender's category before, for undo.
#[tauri::command]
pub fn set_sender_category(
    mailbox: State<'_, Mailbox>,
    address: String,
    category: Option<Category>,
) -> Result<Option<Category>, String> {
    if !mailbox.live() {
        return Ok(mailbox.demo.set_sender_category(&address, category));
    }
    mailbox
        .live
        .engine
        .store()
        .set_sender_category(&address, category)
        .map_err(text)
}

/// Mutes or unmutes a conversation: new mail in it doesn't notify.
#[tauri::command]
pub fn set_muted(
    mailbox: State<'_, Mailbox>,
    conversation: i64,
    muted: bool,
) -> Result<(), String> {
    if !mailbox.live() {
        mailbox.demo.set_muted(conversation, muted);
        return Ok(());
    }
    mailbox
        .live
        .engine
        .store()
        .set_muted(conversation, muted)
        .map_err(text)
}

/// Unsubscribes from the mailing list a message came from. An email the
/// list asks for goes out from the message's account like any other.
#[tauri::command]
pub async fn unsubscribe(mailbox: State<'_, Mailbox>, id: i64) -> Result<Unsubscribed, String> {
    if !mailbox.live() {
        return Ok(Unsubscribed::Done);
    }
    let result = mailbox.live.engine.unsubscribe(id).await.map_err(text)?;
    if let Unsubscribed::Email { to, subject, body } = &result {
        let account = mailbox
            .live
            .engine
            .store()
            .message(id)
            .map_err(text)?
            .ok_or_else(|| "That email is no longer here.".to_owned())?
            .account_id;
        let draft = Draft {
            account,
            to: to.clone(),
            subject: subject.clone(),
            body: body.clone(),
            send_at: now_ms(),
            ..Default::default()
        };
        let json = serde_json::to_string(&draft).map_err(text)?;
        mailbox
            .live
            .engine
            .store()
            .queue_send(account, &json, draft.send_at, &[])
            .map_err(text)?;
        mailbox.live.nudge(account);
    }
    Ok(result)
}

/// Deletes messages for good on their servers. Returns how many went.
#[tauri::command]
pub async fn delete_forever(
    app: AppHandle,
    mailbox: State<'_, Mailbox>,
    ids: Vec<i64>,
) -> Result<usize, String> {
    if !mailbox.live() {
        return Ok(mailbox.demo.delete_forever(&ids));
    }
    let removed = mailbox.live.engine.delete_forever(&ids).await;
    // Some may have gone before one failed.
    let _ = app.emit("mailbox-changed", ());
    removed.map_err(text)
}

/// Deletes everything in these folders (of any accounts) for good on their
/// servers. Returns how many emails went.
#[tauri::command]
pub async fn empty_folders(
    app: AppHandle,
    mailbox: State<'_, Mailbox>,
    ids: Vec<i64>,
) -> Result<usize, String> {
    if !mailbox.live() {
        return Ok(mailbox.demo.empty_folders(&ids));
    }
    let mut emptied = 0;
    let mut failed = None;
    for id in ids {
        match mailbox.live.engine.empty_folder(id).await {
            Ok(count) => emptied += count,
            Err(e) => failed = Some(e),
        }
    }
    let _ = app.emit("mailbox-changed", ());
    match failed {
        Some(e) => Err(text(e)),
        None => Ok(emptied),
    }
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
            html: demo::html(id),
            to: if m.to.is_empty() {
                vec!["You".into()]
            } else {
                m.to
            },
            cc: Vec::new(),
            reply_to: m.reply_to,
            attachments: m
                .attachment
                .into_iter()
                .map(|name| AttachmentView {
                    mime: mime_for(&name).into(),
                    name,
                    size: 0,
                })
                .collect(),
            invite: demo::invite(
                id,
                &demo::accounts()
                    .into_iter()
                    .find(|a| a.id == m.account)
                    .map(|a| a.email)
                    .unwrap_or_default(),
                mailbox.demo.answer(id),
            ),
        });
    }
    let store = mailbox.live.engine.store();
    let content = store
        .content(id)
        .map_err(text)?
        .ok_or_else(|| format!("no message with id {id}"))?;
    let invite = match content.invite.as_deref().and_then(Invite::from_json) {
        Some(invite) => {
            let account = store.message(id).map_err(text)?.map(|m| m.account_id);
            let me = match account {
                Some(account) => store.account(account).map_err(text)?.map(|a| a.email),
                None => None,
            };
            let sent = content.rsvp.as_deref().and_then(Answer::parse);
            Some(invite.card(&me.unwrap_or_default(), sent))
        }
        None => None,
    };
    Ok(MessageContent {
        text: paragraphs(&content.text),
        html: content.html,
        to: content.to,
        cc: content.cc,
        reply_to: content.reply_to,
        attachments: content
            .attachments
            .into_iter()
            .map(|a| AttachmentView {
                name: a.name,
                mime: a.mime,
                size: a.size,
            })
            .collect(),
        invite,
    })
}

/// Answers the invitation a message carries, by email to its organizer.
/// Resolves once the reply is sent.
#[tauri::command]
pub async fn answer_invite(
    mailbox: State<'_, Mailbox>,
    id: i64,
    answer: Answer,
) -> Result<(), String> {
    if !mailbox.live() {
        mailbox.demo.set_answer(id, answer);
        return Ok(());
    }
    mailbox
        .live
        .engine
        .answer_invite(id, answer)
        .await
        .map_err(text)
}

/// The stored name of attachment `index`, checked before downloading.
fn attachment_name(mailbox: &Mailbox, id: i64, index: usize) -> Result<String, String> {
    if !mailbox.live() {
        return Err(DEMO_FILES.into());
    }
    let content = mailbox.live.engine.store().content(id).map_err(text)?;
    content
        .and_then(|c| c.attachments.into_iter().nth(index))
        .map(|a| a.name)
        .ok_or_else(|| "That attachment isn't part of the email.".to_owned())
}

async fn download(mailbox: &Mailbox, id: i64, index: usize) -> Result<AttachmentData, String> {
    mailbox
        .live
        .engine
        .attachment(id, index)
        .await
        .map_err(text)
}

/// Downloads an attachment and opens it in its default app.
#[tauri::command]
pub async fn open_attachment(
    mailbox: State<'_, Mailbox>,
    id: i64,
    index: usize,
) -> Result<(), String> {
    let name = attachment_name(&mailbox, id, index)?;
    if files::is_risky(&name) {
        return Err(format!(
            "{name} could run a program, so spark-rs won't open it. Save it and check it first."
        ));
    }
    let file = download(&mailbox, id, index).await?;
    let dir = files::opened_dir().join(format!("{id}-{index}"));
    std::fs::create_dir_all(&dir).map_err(text)?;
    let path = dir.join(files::safe_file_name(&file.name));
    files::write_downloaded(&path, &file.bytes)?;
    open::that_detached(&path).map_err(|e| format!("Couldn't open {}: {e}", file.name))
}

/// Asks where to save an attachment, then downloads it there. False if the
/// user cancelled.
#[tauri::command]
pub async fn save_attachment(
    window: Window,
    mailbox: State<'_, Mailbox>,
    id: i64,
    index: usize,
) -> Result<bool, String> {
    let name = attachment_name(&mailbox, id, index)?;
    let Some(target) = rfd::AsyncFileDialog::new()
        .set_parent(&window)
        .set_title("Save attachment")
        .set_file_name(files::safe_file_name(&name))
        .save_file()
        .await
    else {
        return Ok(false);
    };
    let file = download(&mailbox, id, index).await?;
    files::write_downloaded(target.path(), &file.bytes)?;
    Ok(true)
}

/// Asks for a folder and saves every attachment there. Returns how many were
/// saved; 0 if the user cancelled.
#[tauri::command]
pub async fn save_all_attachments(
    window: Window,
    mailbox: State<'_, Mailbox>,
    id: i64,
) -> Result<usize, String> {
    if !mailbox.live() {
        return Err(DEMO_FILES.into());
    }
    let count = mailbox
        .live
        .engine
        .store()
        .content(id)
        .map_err(text)?
        .map_or(0, |c| c.attachments.len());
    let Some(folder) = rfd::AsyncFileDialog::new()
        .set_parent(&window)
        .set_title("Save attachments to")
        .pick_folder()
        .await
    else {
        return Ok(0);
    };
    let folder: PathBuf = folder.path().to_owned();
    for index in 0..count {
        let file = download(&mailbox, id, index).await?;
        let path = files::unique_path(&folder, &files::safe_file_name(&file.name));
        files::write_downloaded(&path, &file.bytes)?;
    }
    Ok(count)
}

/// The images an email embeds with `cid:` URLs, as data URLs by Content-ID
/// (lowercase, without angle brackets). Empty for demo mail.
#[tauri::command]
pub async fn inline_images(
    mailbox: State<'_, Mailbox>,
    id: i64,
) -> Result<HashMap<String, String>, String> {
    if !mailbox.live() {
        return Ok(HashMap::new());
    }
    let images = mailbox.live.engine.inline_images(id).await.map_err(text)?;
    Ok(images
        .iter()
        .map(|i| (i.cid.clone(), i.data_url()))
        .collect())
}

/// The file picker for attaching files.
#[tauri::command]
pub async fn pick_files(window: Window) -> Vec<LocalFile> {
    let picked = rfd::AsyncFileDialog::new()
        .set_parent(&window)
        .set_title("Attach files")
        .pick_files()
        .await
        .unwrap_or_default();
    picked
        .iter()
        .filter_map(|f| files::describe(f.path()))
        .collect()
}

/// Names and sizes of dropped files; folders are skipped.
#[tauri::command]
pub fn describe_files(paths: Vec<String>) -> Vec<LocalFile> {
    paths
        .iter()
        .filter_map(|p| files::describe(p.as_ref()))
        .collect()
}

/// Picture types the composer's body shows; what browsers show and mail
/// clients take.
const BODY_IMAGES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/bmp",
];

/// An image file dropped on the composer's body, as a data URL to show in
/// it. Anything that isn't a picture, or is over the 25 MB servers take,
/// is refused.
#[tauri::command]
pub async fn read_image(path: String) -> Result<String, String> {
    let mime = mime_for(&path);
    if !BODY_IMAGES.contains(&mime) {
        return Err("That isn't a picture.".into());
    }
    let size = tokio::fs::metadata(&path).await.map_err(text)?.len();
    if size > draft::MAX_ATTACHMENTS_BYTES {
        return Err("That picture is over the 25 MB mail servers take.".into());
    }
    let bytes = tokio::fs::read(&path).await.map_err(text)?;
    Ok(InlineImage {
        cid: String::new(),
        mime: mime.into(),
        bytes,
    }
    .data_url())
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

/// Accepts or blocks senders or domains for every account (`None` makes
/// them new again), moving their mail to match. Returns the changes that
/// undo it.
#[tauri::command]
pub fn decide_senders(
    mailbox: State<'_, Mailbox>,
    changes: Vec<Change>,
) -> Result<Vec<Change>, String> {
    if !mailbox.live() {
        return mailbox.demo.decide(&changes);
    }
    mailbox
        .live
        .engine
        .store()
        .decide(&changes, now_ms())
        .map_err(text)
}

/// What's been decided about an address and its domain, e.g. for the card
/// on a blocked sender's email.
#[tauri::command]
pub fn sender_status(mailbox: State<'_, Mailbox>, address: String) -> Result<SenderStatus, String> {
    if !mailbox.live() {
        return Ok(mailbox.demo.sender_status(&address));
    }
    mailbox
        .live
        .engine
        .store()
        .sender_status(&address)
        .map_err(text)
}

#[tauri::command]
pub fn gatekeeper_mode(mailbox: State<'_, Mailbox>) -> Result<GatekeeperMode, String> {
    if !mailbox.live() {
        return Ok(mailbox.demo.gatekeeper_mode());
    }
    mailbox.live.engine.store().gatekeeper_mode().map_err(text)
}

/// Turning the Gatekeeper off accepts everyone waiting in it.
#[tauri::command]
pub fn set_gatekeeper_mode(
    mailbox: State<'_, Mailbox>,
    mode: GatekeeperMode,
) -> Result<(), String> {
    if !mailbox.live() {
        mailbox.demo.set_gatekeeper_mode(mode);
        return Ok(());
    }
    mailbox
        .live
        .engine
        .store()
        .set_gatekeeper_mode(mode)
        .map_err(text)
}

/// The Priority senders, whose mail is all Priority.
#[tauri::command]
pub fn priority_senders(mailbox: State<'_, Mailbox>) -> Result<Vec<String>, String> {
    if !mailbox.live() {
        return Ok(mailbox.demo.priority_senders());
    }
    mailbox.live.engine.store().priority_senders().map_err(text)
}

/// Adds senders to the Priority senders, or takes them off, and marks their
/// mail Priority or not. Returns what `undo_prioritize` puts back.
#[tauri::command]
pub fn prioritize_senders(
    mailbox: State<'_, Mailbox>,
    addresses: Vec<String>,
    priority: bool,
) -> Result<PriorityUndo, String> {
    if !mailbox.live() {
        return mailbox.demo.prioritize_senders(&addresses, priority);
    }
    mailbox
        .live
        .engine
        .store()
        .prioritize_senders(&addresses, priority)
        .map_err(text)
}

#[tauri::command]
pub fn undo_prioritize(mailbox: State<'_, Mailbox>, undo: PriorityUndo) -> Result<(), String> {
    if !mailbox.live() {
        mailbox.demo.undo_prioritize(&undo);
        return Ok(());
    }
    mailbox
        .live
        .engine
        .store()
        .undo_prioritize(&undo)
        .map_err(text)
}

fn prepare_account_add(app: &AppHandle, mailbox: &Mailbox) -> Result<(), String> {
    app.state::<crate::prefs::PrefsState>()
        .prepare_account_add(|| {
            mailbox
                .live
                .engine
                .store()
                .accounts()
                .map(|accounts| accounts.into_iter().map(|account| account.id).collect())
                .map_err(text)
        })
}

#[tauri::command]
pub async fn add_gmail_account(
    app: AppHandle,
    mailbox: State<'_, Mailbox>,
    email: String,
    app_password: String,
    label: String,
    trusted_imap: Option<String>,
    trusted_smtp: Option<String>,
) -> Result<AccountView, Failure> {
    prepare_account_add(&app, &mailbox)?;
    let (mut imap, mut smtp) = gmail_servers();
    imap.trusted_cert = trusted_imap;
    smtp.trusted_cert = trusted_smtp;
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
        .await?;
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
    trusted_imap: Option<String>,
    trusted_smtp: Option<String>,
) -> Result<AccountView, Failure> {
    if imap_host.trim().is_empty() || smtp_host.trim().is_empty() {
        return Err("Enter the IMAP and SMTP servers.".to_owned().into());
    }
    prepare_account_add(&app, &mailbox)?;
    let imap = Server {
        trusted_cert: trusted_imap,
        ..Server::new(&imap_host, imap_port)
    };
    let smtp = Server {
        trusted_cert: trusted_smtp,
        ..Server::new(&smtp_host, smtp_port)
    };
    let account = mailbox
        .live
        .engine
        .add_password_account(AccountKind::Imap, &email, &password, &label, imap, smtp)
        .await?;
    Ok(mailbox.added(&app, account))
}

/// Trusts the certificate an IMAP account's server showed, where the system
/// doesn't, and checks for mail with it.
#[tauri::command]
pub fn trust_certificate(
    mailbox: State<'_, Mailbox>,
    account: i64,
    service: Service,
    fingerprint: String,
) -> Result<(), String> {
    if !mailbox.live() {
        return Err("Demo accounts have no servers.".into());
    }
    mailbox
        .live
        .engine
        .trust_certificate(account, service, &fingerprint)
        .map_err(text)?;
    mailbox.live.sync_now();
    Ok(())
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
    prepare_account_add(&app, &mailbox)?;
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
pub fn remove_account(
    app: AppHandle,
    mailbox: State<'_, Mailbox>,
    prefs: State<'_, crate::prefs::PrefsState>,
    id: i64,
) -> Result<(), String> {
    prefs.revoke_agent_access_before(id, || {
        mailbox.live.stop(id);
        mailbox.live.engine.remove_account(id).map_err(text)
    })?;
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
        // The demo pretends.
        let id = mailbox.demo.send(&draft);
        signed(&mailbox, &draft)?;
        return Ok(id);
    }
    // Catch typos and oversized attachments now rather than after the undo window.
    for field in [&draft.to, &draft.cc, &draft.bcc] {
        draft::recipients(field).map_err(text)?;
    }
    draft.check_size().map_err(text)?;
    // One send per saved draft: another would send it twice.
    if let Some(saved) = draft.draft_id
        && mailbox
            .live
            .engine
            .store()
            .draft_held(saved)
            .map_err(text)?
    {
        return Err("This draft is already on its way.".into());
    }
    // Copies of local files go into the outbox, so a later send doesn't
    // depend on the originals.
    let json = serde_json::to_string(&draft).map_err(text)?;
    let mut store = mailbox.live.engine.store();
    let queued = local_files(&draft, &store)?;
    let id = store
        .queue_send(draft.account, &json, draft.send_at, &queued)
        .map_err(text)?;
    // The saved draft it came from waits with it, out of the lists.
    if let Some(saved) = draft.draft_id {
        store.hold_draft_for_send(id, saved).map_err(text)?;
    }
    drop(store);
    signed(&mailbox, &draft)?;
    if draft.send_at <= now_ms() {
        mailbox.live.nudge(draft.account);
    }
    Ok(id)
}

/// A saved signature going out becomes its address's most recent.
fn signed(mailbox: &Mailbox, draft: &Draft) -> Result<(), String> {
    let (Some(id), Some(_)) = (draft.signature_id, draft.clean_signature()) else {
        return Ok(());
    };
    let Some(address) = mailbox.account_email(draft.account) else {
        return Ok(());
    };
    mailbox
        .live
        .engine
        .store()
        .use_signature(&address, id, now_ms())
        .map_err(text)
}

/// True if the send was stopped in time. A saved draft it came from is
/// listed again.
#[tauri::command]
pub fn cancel_send(mailbox: State<'_, Mailbox>, id: i64) -> Result<bool, String> {
    if !mailbox.live() {
        return Ok(mailbox.demo.cancel_send(id));
    }
    mailbox.live.engine.store().cancel_send(id).map_err(text)
}

/// Copies of a draft's files from this computer, in order. A recovered draft
/// can use its saved bytes after the original file has moved or disappeared.
fn local_files(draft: &Draft, store: &spark_store::Store) -> Result<Vec<OutboxFile>, String> {
    let mut files = Vec::new();
    let mut saved: Option<(Draft, Vec<OutboxFile>)> = None;
    for a in &draft.attachments {
        let OutgoingAttachment::File { path, name, size } = a else {
            continue;
        };
        let file = match std::fs::read(path) {
            Ok(data) => OutboxFile {
                name: name.clone(),
                mime: mime_for(name).into(),
                data,
            },
            Err(e) => {
                let missing = || format!("Couldn't read {name}: {e}");
                if saved.is_none() {
                    let id = draft.draft_id.ok_or_else(missing)?;
                    let json = store.draft(id).map_err(text)?.ok_or_else(missing)?;
                    saved = Some((
                        serde_json::from_str(&json).map_err(text)?,
                        store.draft_files(id).map_err(text)?,
                    ));
                }
                let (original, copies) = saved.as_ref().expect("loaded above");
                // Copies contain only File attachments. Locate this exact
                // saved attachment rather than substituting a removed file
                // when the composer changed their order or added another.
                let position = original
                    .attachments
                    .iter()
                    .filter(|a| matches!(a, OutgoingAttachment::File { .. }))
                    .position(|original| original == a)
                    .ok_or_else(missing)?;
                copies
                    .get(position)
                    .filter(|f| f.name == *name && f.data.len() as u64 == *size)
                    .cloned()
                    .ok_or_else(missing)?
            }
        };
        files.push(file);
    }
    Ok(files)
}

/// Saves the composer's draft in its account's Drafts folder: here at once,
/// on the server with the account's next round, replacing the copy it was
/// opened from. Returns the draft's message id, for saving it again.
#[tauri::command]
pub fn save_draft(mailbox: State<'_, Mailbox>, draft: Draft) -> Result<i64, String> {
    if !mailbox.live() {
        return mailbox.demo.save_draft(draft);
    }
    let files = local_files(&draft, &mailbox.live.engine.store())?;
    mailbox.live.engine.save_draft(&draft, &files).map_err(text)
}

/// A draft from Drafts, for the composer.
#[tauri::command]
pub async fn open_draft(mailbox: State<'_, Mailbox>, id: i64) -> Result<Draft, String> {
    let gone = || "That draft is gone.".to_owned();
    if !mailbox.live() {
        return mailbox.demo.open_draft(id).ok_or_else(gone);
    }
    let mut draft = mailbox
        .live
        .engine
        .open_draft(id)
        .map_err(text)?
        .ok_or_else(gone)?;
    // A copy from the server shows its pictures by cid: they come back as
    // data, for the editor to show and a send to carry again.
    let shown = [draft.body_html.as_deref(), draft.signature.as_deref()]
        .into_iter()
        .flatten()
        .any(|html| !cid_refs(html).is_empty());
    if shown {
        match mailbox.live.engine.inline_images(id).await {
            Ok(images) => {
                for html in [&mut draft.body_html, &mut draft.signature]
                    .into_iter()
                    .flatten()
                {
                    *html = signature::inline_parts(html, &images);
                }
            }
            Err(e) => eprintln!("spark-rs: the draft's pictures didn't load: {e}"),
        }
    }
    Ok(draft)
}

/// Deletes a draft, here and on the server.
#[tauri::command]
pub fn discard_draft(mailbox: State<'_, Mailbox>, id: i64) -> Result<(), String> {
    if !mailbox.live() {
        mailbox.demo.discard_draft(id);
        return Ok(());
    }
    mailbox
        .live
        .engine
        .store()
        .discard_draft(id)
        .map_err(text)?;
    Ok(())
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
    use spark_store::{NewAccount, Store};

    fn attachment_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("spark-draft-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn draft_account(store: &Store) -> Account {
        store
            .add_account(&NewAccount {
                kind: AccountKind::Imap,
                email: "me@example.test",
                name: "Me",
                label: "Test",
                // An invalid server configuration fails locally, without network access.
                config: "{}",
            })
            .unwrap()
    }

    fn file_attachment(path: &std::path::Path, size: u64) -> OutgoingAttachment {
        OutgoingAttachment::File {
            path: path.to_string_lossy().into_owned(),
            name: path.file_name().unwrap().to_string_lossy().into_owned(),
            size,
        }
    }

    #[tokio::test]
    async fn a_recovered_draft_can_save_and_send_after_its_original_file_is_deleted() {
        struct NoSecrets;
        impl spark_mail::Secrets for NoSecrets {
            fn get(&self, _: &str) -> Result<Option<String>, String> {
                Ok(None)
            }
            fn set(&self, _: &str, _: &str) -> Result<(), String> {
                Ok(())
            }
        }
        let mut store = Store::open_in_memory().unwrap();
        let account = draft_account(&store);
        let dir = attachment_dir("recovered-files");
        let path = dir.join("plan.txt");
        std::fs::write(&path, b"saved attachment").unwrap();
        let draft = Draft {
            account: account.id,
            to: "recipient@example.test".into(),
            body: "Please keep this".into(),
            attachments: vec![file_attachment(&path, 16)],
            ..Default::default()
        };
        let files = local_files(&draft, &store).unwrap();
        let queued = store
            .queue_send(
                account.id,
                &serde_json::to_string(&draft).unwrap(),
                0,
                &files,
            )
            .unwrap();
        std::fs::remove_file(&path).unwrap();
        let engine = spark_mail::Engine::new(
            std::sync::Arc::new(std::sync::Mutex::new(store)),
            std::sync::Arc::new(NoSecrets),
        );
        let result = engine.send_due(account.id).await.unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].result.is_err());
        let mut store = engine.store();
        assert!(store.outbox_files(queued).unwrap().is_empty());
        let id = store.messages().unwrap()[0].id;
        let recovered = spark_mail::drafts::open(&store, id).unwrap().unwrap();
        let copied = local_files(&recovered, &store).unwrap();
        assert_eq!(copied, files);
        let id =
            spark_mail::drafts::save(&mut store, &account, &recovered, &copied, now_ms()).unwrap();
        assert_eq!(store.draft_files(id).unwrap(), files);
        let retry = store
            .queue_send(
                account.id,
                &serde_json::to_string(&recovered).unwrap(),
                0,
                &copied,
            )
            .unwrap();
        assert_eq!(store.outbox_files(retry).unwrap(), files);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn saved_files_match_the_original_attachment_after_reordering() {
        let mut store = Store::open_in_memory().unwrap();
        let account = draft_account(&store);
        let dir = attachment_dir("file-identity");
        let first = dir.join("first.txt");
        let second = dir.join("second.txt");
        std::fs::write(&first, b"one").unwrap();
        std::fs::write(&second, b"two").unwrap();
        let draft = Draft {
            account: account.id,
            attachments: vec![
                file_attachment(&first, 3),
                OutgoingAttachment::Message {
                    message: 42,
                    index: 0,
                    name: "remote.txt".into(),
                    size: 1,
                },
                file_attachment(&second, 3),
            ],
            ..Default::default()
        };
        let files = local_files(&draft, &store).unwrap();
        let id = spark_mail::drafts::save(&mut store, &account, &draft, &files, now_ms()).unwrap();
        std::fs::remove_file(&first).unwrap();
        std::fs::remove_file(&second).unwrap();
        let mut opened = spark_mail::drafts::open(&store, id).unwrap().unwrap();
        opened.attachments.remove(0);
        assert_eq!(
            local_files(&opened, &store).unwrap(),
            vec![files[1].clone()]
        );
        // The same name and size at a different path is a new attachment.
        opened.attachments[1] = file_attachment(&dir.join("elsewhere/second.txt"), 3);
        assert!(local_files(&opened, &store).is_err());
        // When its original exists, use its current bytes rather than an old copy.
        opened.attachments[1] = file_attachment(&second, 3);
        std::fs::write(&second, b"new").unwrap();
        assert_eq!(local_files(&opened, &store).unwrap()[0].data, b"new");
        std::fs::remove_dir_all(dir).unwrap();
    }

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
            reply_to: vec!["Help <help@x.com>".into()],
            subject: "Hi".into(),
            snippet: "…".into(),
            date: 9,
            attachment: None,
            folders: vec![4],
            folder: None,
            to: vec!["Me <me@x.com>".into()],
            conversation: 2,
            returned_at: None,
            muted: true,
            reminder: None,
            invite_kind: None,
        });
        let json = serde_json::to_value(&m).unwrap();
        assert_eq!(json["muted"], true);
        assert!(json.get("returnedAt").is_none());
        assert_eq!(json["account"], 7);
        assert_eq!(json["from"], "Ana");
        assert_eq!(json["address"], "ana@x.com");
        assert_eq!(json["replyTo"], serde_json::json!(["Help <help@x.com>"]));
        assert_eq!(json["timestamp"], 9);
        assert_eq!(json["snoozedUntil"], 5);
        assert_eq!(json["category"], "newsletters");
        assert_eq!(json["state"], "snoozed");
        assert!(json.get("body").is_none() && json.get("attachment").is_none());
        assert_eq!(json["folders"], serde_json::json!([4]));
        assert!(json.get("folder").is_none());
        assert_eq!(json["to"], serde_json::json!(["Me <me@x.com>"]));
    }
}
