//! What the UI talks to: real accounts once one is added, the demo mailbox
//! until then. Types here are the UI's shapes (`src/types.ts` mirrors them).

use std::collections::{BTreeMap, HashMap};
use std::io::{self, Read as _};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use serde::Serialize;
use spark_mail::attachment::{AttachmentData, InlineImage, cid_refs, mime_for};
use spark_mail::draft::{self, Draft, OutgoingAttachment};
use spark_mail::engine::{folder_has_older, gmail_servers, inbox_has_older};
use spark_mail::imap::Server;
use spark_mail::invite::{Answer, Card, Invite};
use spark_mail::unsubscribe::Unsubscribed;
use spark_mail::{Error, Service, Untrusted};
use spark_mail::{signature, trackers};
use spark_store::{
    Account, AccountKind, Category, Change, Contact, DraftFile, DueReminder, Folder, FolderRole,
    GatekeeperMode, MessageState, OutboxFile, Patch, PriorityUndo, SavedFile, SenderStatus, Store,
    now_ms,
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

/// The ids of the stored messages whose words start with those of `query`,
/// in no particular order. None in the demo, where the UI matches the
/// messages it has. A short word can find most of the mail, so it's looked
/// up off the main thread.
#[tauri::command]
pub async fn search_messages(
    mailbox: State<'_, Mailbox>,
    query: String,
) -> Result<Option<Vec<i64>>, String> {
    if !mailbox.live() {
        return Ok(None);
    }
    let engine = mailbox.live.engine.clone();
    let found = tauri::async_runtime::spawn_blocking(move || engine.store().search_ids(&query))
        .await
        .map_err(text)?
        .map_err(text)?;
    Ok(Some(found))
}

/// How many people a recipient field suggests at most.
const SUGGESTIONS: usize = 20;

/// The people to suggest for what's typed in a recipient field, best first.
/// After mail changes, finding them again reads all of it, so it's done off
/// the main thread.
#[tauri::command]
pub async fn suggest_contacts(
    mailbox: State<'_, Mailbox>,
    query: String,
) -> Result<Vec<Contact>, String> {
    if !mailbox.live() {
        return Ok(mailbox.demo.suggest_contacts(&query, SUGGESTIONS));
    }
    let engine = mailbox.live.engine.clone();
    tauri::async_runtime::spawn_blocking(move || {
        engine
            .store()
            .suggest_contacts(&query, now_ms(), SUGGESTIONS)
    })
    .await
    .map_err(text)?
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
        html: content.html.map(|html| trackers::strip(&html).into_owned()),
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

/// Attachment `index`: the copy kept since it was last downloaded, if
/// there is one.
async fn download(mailbox: &Mailbox, id: i64, index: usize) -> Result<AttachmentData, String> {
    mailbox
        .live
        .engine
        .attachment(id, index)
        .await
        .map_err(text)
}

/// Runs file work on a blocking thread, off the async ones.
async fn on_disk(work: impl FnOnce() -> Result<(), String> + Send + 'static) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(text)?
}

/// What opening an attachment did.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Opened {
    Opened,
    /// It could run a program: Spark asks first ("Open Anyway").
    Risky {
        name: String,
    },
}

/// Downloads an attachment and opens it in its default app. One that could
/// run a program only opens with `anyway`, once the user has said so; it's
/// marked as downloaded from the internet either way, for Windows to check.
#[tauri::command]
pub async fn open_attachment(
    mailbox: State<'_, Mailbox>,
    id: i64,
    index: usize,
    anyway: Option<bool>,
) -> Result<Opened, String> {
    let name = attachment_name(&mailbox, id, index)?;
    if files::is_risky(&name) && anyway != Some(true) {
        return Ok(Opened::Risky { name });
    }
    let file = download(&mailbox, id, index).await?;
    let path = files::opened_dir()
        .join(format!("{id}-{index}"))
        .join(files::safe_file_name(&file.name));
    let opened = path.clone();
    let name = file.name.clone();
    on_disk(move || files::write_opened(&opened, &file.bytes)).await?;
    open::that_detached(&path).map_err(|e| format!("Couldn't open {name}: {e}"))?;
    Ok(Opened::Opened)
}

/// Spark's Auto-download: fetches an email's attachments ahead, into the
/// cache, so they open at once. Returns how many it fetched.
#[tauri::command]
pub async fn prefetch_attachments(mailbox: State<'_, Mailbox>, id: i64) -> Result<usize, String> {
    if !mailbox.live() {
        return Ok(0);
    }
    mailbox
        .live
        .engine
        .prefetch_attachments(id)
        .await
        .map_err(text)
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
    let target = target.path().to_owned();
    on_disk(move || files::write_downloaded(&target, &file.bytes)).await?;
    Ok(true)
}

/// Asks where to save an email, then writes it there as it is on the server
/// (an .eml file other mail apps open). False if the user cancelled.
#[tauri::command]
pub async fn save_eml(
    window: Window,
    mailbox: State<'_, Mailbox>,
    id: i64,
    name: String,
) -> Result<bool, String> {
    if !mailbox.live() {
        return Err(DEMO_FILES.into());
    }
    let Some(target) = rfd::AsyncFileDialog::new()
        .set_parent(&window)
        .set_title("Save email")
        .add_filter("Email", &["eml"])
        .set_file_name(files::safe_file_name(&format!("{}.eml", name.trim())))
        .save_file()
        .await
    else {
        return Ok(false);
    };
    let raw = mailbox.live.engine.raw_message(id).await.map_err(text)?;
    let target = target.path().to_owned();
    on_disk(move || files::write_downloaded(&target, &raw)).await?;
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
    // All in one go: for IMAP, one download of the email.
    let all: Vec<usize> = (0..count).collect();
    let downloaded = mailbox
        .live
        .engine
        .attachments(id, &all)
        .await
        .map_err(text)?;
    on_disk(move || {
        for file in &downloaded {
            let path = files::unique_path(&folder, &files::safe_file_name(&file.name));
            files::write_downloaded(&path, &file.bytes)?;
        }
        Ok(())
    })
    .await?;
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
pub async fn send_message(mailbox: State<'_, Mailbox>, draft: Draft) -> Result<i64, String> {
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
    let engine = mailbox.live.engine.clone();
    let queued = draft.clone();
    let id = tauri::async_runtime::spawn_blocking(move || queue_live(&engine, &queued))
        .await
        .map_err(text)??;
    signed(&mailbox, &draft)?;
    if draft.send_at <= now_ms() {
        mailbox.live.nudge(draft.account);
    }
    Ok(id)
}

/// Queues a real account's draft. Copies of its files from this computer go
/// into the outbox, so a later send doesn't depend on the originals; they're
/// read before the store is locked.
fn queue_live(engine: &spark_mail::Engine, draft: &Draft) -> Result<i64, String> {
    let json = serde_json::to_string(draft).map_err(text)?;
    let read: Vec<io::Result<Vec<u8>>> = attached(draft)
        .iter()
        .map(|f| std::fs::read(f.path))
        .collect();
    let mut store = engine.store();
    // One send per saved draft: another would send it twice.
    if let Some(saved) = draft.draft_id
        && store.draft_held(saved).map_err(text)?
    {
        return Err("This draft is already on its way.".into());
    }
    let queued = outbox_files(draft, read, &store)?;
    let id = store
        .queue_send(draft.account, &json, draft.send_at, &queued)
        .map_err(text)?;
    // The saved draft it came from waits with it, out of the lists.
    if let Some(saved) = draft.draft_id {
        store.hold_draft_for_send(id, saved).map_err(text)?;
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

/// A file from this computer attached to a draft.
struct Attached<'a> {
    attachment: &'a OutgoingAttachment,
    path: &'a str,
    name: &'a str,
    size: u64,
}

/// A draft's files from this computer, in order.
fn attached(draft: &Draft) -> Vec<Attached<'_>> {
    draft
        .attachments
        .iter()
        .filter_map(|a| match a {
            OutgoingAttachment::File { path, name, size } => Some(Attached {
                attachment: a,
                path,
                name,
                size: *size,
            }),
            OutgoingAttachment::Message { .. } => None,
        })
        .collect()
}

/// The draft as last saved, and its id, for files that can't be read from
/// this computer: a recovered draft can use its saved bytes after the
/// original file has moved or disappeared. `missing` is the error without it.
fn saved_draft(
    store: &Store,
    draft: &Draft,
    missing: &dyn Fn() -> String,
) -> Result<(i64, Draft), String> {
    let id = draft.draft_id.ok_or_else(missing)?;
    let json = store.draft(id).map_err(text)?.ok_or_else(missing)?;
    Ok((id, serde_json::from_str(&json).map_err(text)?))
}

/// Where the draft as saved has exactly this attachment among its files:
/// the first place not `used` yet, else the first. Locating this exact
/// attachment, rather than one in its place, keeps a removed file from
/// standing in when the composer changed their order or added another.
fn saved_position(original: &Draft, file: &Attached, used: &[bool]) -> Option<usize> {
    let same: Vec<usize> = attached(original)
        .iter()
        .enumerate()
        .filter(|(_, o)| o.attachment == file.attachment)
        .map(|(p, _)| p)
        .collect();
    let unused = same
        .iter()
        .find(|&&p| !used.get(p).copied().unwrap_or(false));
    unused.or(same.first()).copied()
}

/// Copies of a draft's files for the outbox, in order, given what reading
/// each from this computer gave.
fn outbox_files(
    draft: &Draft,
    read: Vec<io::Result<Vec<u8>>>,
    store: &Store,
) -> Result<Vec<OutboxFile>, String> {
    let mut files = Vec::new();
    let mut saved: Option<(Draft, Vec<OutboxFile>)> = None;
    let mut used = Vec::new();
    for (file, read) in attached(draft).iter().zip(read) {
        let name = file.name;
        let copy = match read {
            Ok(data) => OutboxFile {
                name: name.to_owned(),
                mime: mime_for(name).into(),
                data,
            },
            Err(e) => {
                let missing = || format!("Couldn't read {name}: {e}");
                if saved.is_none() {
                    let (id, original) = saved_draft(store, draft, &missing)?;
                    let copies = store.draft_files(id).map_err(text)?;
                    used = vec![false; copies.len()];
                    saved = Some((original, copies));
                }
                let (original, copies) = saved.as_ref().expect("loaded above");
                let position = saved_position(original, file, &used).ok_or_else(missing)?;
                used[position] = true;
                copies
                    .get(position)
                    .filter(|f| f.name == name && f.data.len() as u64 == file.size)
                    .cloned()
                    .ok_or_else(missing)?
            }
        };
        files.push(copy);
    }
    Ok(files)
}

/// What a file from this computer was when a draft saved it. A save while
/// its size and modified time are the same keeps the saved copy rather than
/// reading it again; a file replaced or written since changes them.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Source {
    path: String,
    len: u64,
    modified: SystemTime,
}

impl Source {
    fn of(path: &str, meta: &std::fs::Metadata) -> Option<Self> {
        Some(Self {
            path: path.to_owned(),
            len: meta.len(),
            modified: meta.modified().ok()?,
        })
    }

    /// The file at `path` as it is now.
    fn now(path: &str) -> Option<Self> {
        Self::of(path, &std::fs::metadata(path).ok()?)
    }
}

/// A modified time this close to a read isn't trusted: the coarsest (FAT's,
/// two seconds) can't tell a write just before the read from one just after.
const SETTLED: Duration = Duration::from_secs(2);

/// A copy of a file read from this computer, and its source if a later save
/// can trust the file unchanged by it.
struct LocalCopy {
    file: OutboxFile,
    source: Option<Source>,
}

/// Reads a file attached from this computer.
fn read_local(path: &str, name: &str) -> io::Result<LocalCopy> {
    let started = SystemTime::now();
    let mut file = std::fs::File::open(path)?;
    let before = file.metadata()?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;
    // Written while it was read, or just before: the next save reads it again.
    let source = Source::of(path, &before).filter(|s| {
        s.len == data.len() as u64
            && file
                .metadata()
                .ok()
                .and_then(|m| Source::of(path, &m))
                .as_ref()
                == Some(s)
            && s.modified
                .checked_add(SETTLED)
                .is_some_and(|t| t <= started)
    });
    Ok(LocalCopy {
        file: OutboxFile {
            name: name.to_owned(),
            mime: mime_for(name).into(),
            data,
        },
        source,
    })
}

/// A draft's saved files as this run of the app last saved them.
#[derive(Clone)]
struct Snapshot {
    /// The save they're from (`Store::draft_saved_at`). Any later save
    /// changes it, a failed send's recovery from the outbox too.
    saved_at: i64,
    /// Where each came from, in order: `None` for one not known to match
    /// a file on this computer.
    sources: Vec<Option<Source>>,
}

/// Snapshots by the draft's message id, so an autosave that changed only
/// the text doesn't read its files again. After a restart, the first save
/// reads them once.
#[derive(Default)]
struct SavedFiles(Mutex<BTreeMap<i64, Snapshot>>);

static SAVED_FILES: SavedFiles = SavedFiles(Mutex::new(BTreeMap::new()));

impl SavedFiles {
    fn lock(&self) -> MutexGuard<'_, BTreeMap<i64, Snapshot>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Draft `id`'s snapshot, if it's still what the store has.
    fn current(
        &self,
        store: &Store,
        id: i64,
        saved: &[SavedFile],
    ) -> Result<Option<Snapshot>, String> {
        let Some(saved_at) = store.draft_saved_at(id).map_err(text)? else {
            return Ok(None);
        };
        Ok(self
            .lock()
            .get(&id)
            .filter(|s| s.saved_at == saved_at && s.sources.len() == saved.len())
            .cloned())
    }

    /// Takes the snapshot of draft `id` just saved, over `was` (the draft it
    /// was saved over).
    fn saved(
        &self,
        store: &Store,
        was: Option<i64>,
        id: i64,
        sources: Vec<Option<Source>>,
    ) -> Result<(), String> {
        let saved_at = store.draft_saved_at(id).map_err(text)?;
        let mut snapshots = self.lock();
        if let Some(was) = was {
            snapshots.remove(&was);
        }
        if let Some(saved_at) = saved_at {
            snapshots.insert(id, Snapshot { saved_at, sources });
        }
        Ok(())
    }

    fn forget(&self, id: i64) {
        self.lock().remove(&id);
    }
}

/// How a save gets one of the draft's files.
enum Pick<'a> {
    Read(&'a LocalCopy),
    /// The copy the draft has saved at this index of its files; and the
    /// file it came from, if that's known.
    Saved(usize, Option<Source>),
}

/// How a save gets the draft's files.
enum Picked<'a> {
    /// Each of them, in order.
    All(Vec<Pick<'a>>),
    /// It has to read these first (by index among them).
    Unread(Vec<usize>),
}

/// How a save gets the draft's files: `found` is each as it is on this
/// computer, `read` what reading it gave, if it was read.
fn pick<'a>(
    store: &Store,
    draft: &Draft,
    saved: &[SavedFile],
    snapshot: Option<&Snapshot>,
    found: &[Option<Source>],
    read: &'a [Option<io::Result<LocalCopy>>],
) -> Result<Picked<'a>, String> {
    let mut picks = Vec::new();
    let mut unread = Vec::new();
    let mut used = vec![false; saved.len()];
    let mut original = None;
    for (k, file) in attached(draft).iter().enumerate() {
        let e = match &read[k] {
            Some(Ok(copy)) => {
                picks.push(Pick::Read(copy));
                continue;
            }
            Some(Err(e)) => Some(e),
            None => None,
        };
        // Unchanged since this run saved it: the copy it has stays.
        let unchanged = snapshot.zip(found[k].as_ref()).and_then(|(snapshot, now)| {
            (0..saved.len()).find(|&p| {
                !used[p]
                    && snapshot.sources[p].as_ref() == Some(now)
                    && saved[p].name == file.name
                    && saved[p].size == now.len
            })
        });
        if let Some(p) = unchanged {
            used[p] = true;
            picks.push(Pick::Saved(p, found[k].clone()));
            continue;
        }
        let Some(e) = e else {
            unread.push(k);
            continue;
        };
        let name = file.name;
        let missing = || format!("Couldn't read {name}: {e}");
        if original.is_none() {
            original = Some(saved_draft(store, draft, &missing)?.1);
        }
        let position = saved_position(original.as_ref().expect("loaded above"), file, &used)
            .filter(|&p| {
                saved
                    .get(p)
                    .is_some_and(|s| s.name == name && s.size == file.size)
            })
            .ok_or_else(missing)?;
        used[position] = true;
        picks.push(Pick::Saved(position, None));
    }
    Ok(if unread.is_empty() {
        Picked::All(picks)
    } else {
        Picked::Unread(unread)
    })
}

/// Saves a real account's draft with its files from this computer. A file
/// unchanged since this run of the app saved it isn't read again; one that
/// can't be read keeps the copy the draft saved. Files are read while the
/// store isn't locked.
fn save_live(
    engine: &spark_mail::Engine,
    snapshots: &SavedFiles,
    draft: &Draft,
    read_file: impl Fn(&str, &str) -> io::Result<LocalCopy>,
) -> Result<i64, String> {
    let local = attached(draft);
    let found: Vec<Option<Source>> = local.iter().map(|f| Source::now(f.path)).collect();
    let mut read: Vec<Option<io::Result<LocalCopy>>> = local.iter().map(|_| None).collect();
    loop {
        let mut store = engine.store();
        let (saved, snapshot) = match draft.draft_id {
            Some(id) => {
                let saved = store.saved_files(id).map_err(text)?;
                let snapshot = snapshots.current(&store, id, &saved)?;
                (saved, snapshot)
            }
            None => (Vec::new(), None),
        };
        let unread = match pick(&store, draft, &saved, snapshot.as_ref(), &found, &read)? {
            Picked::All(picks) => {
                let files: Vec<DraftFile> = picks
                    .iter()
                    .map(|p| match p {
                        Pick::Read(copy) => DraftFile::Copy(&copy.file),
                        Pick::Saved(p, _) => DraftFile::Saved(&saved[*p]),
                    })
                    .collect();
                let account = store
                    .account(draft.account)
                    .map_err(text)?
                    .ok_or_else(|| "That account was removed.".to_owned())?;
                let id = spark_mail::drafts::save(&mut store, &account, draft, &files, now_ms())
                    .map_err(text)?;
                let sources = picks
                    .into_iter()
                    .map(|p| match p {
                        Pick::Read(copy) => copy.source.clone(),
                        Pick::Saved(_, source) => source,
                    })
                    .collect();
                snapshots.saved(&store, draft.draft_id, id, sources)?;
                return Ok(id);
            }
            Picked::Unread(unread) => unread,
        };
        // The store may change meanwhile; what's read is checked against it again.
        drop(store);
        for k in unread {
            read[k] = Some(read_file(local[k].path, local[k].name));
        }
    }
}

/// Saves the composer's draft in its account's Drafts folder: here at once,
/// on the server with the account's next round, replacing the copy it was
/// opened from. Returns the draft's message id, for saving it again.
#[tauri::command]
pub async fn save_draft(mailbox: State<'_, Mailbox>, draft: Draft) -> Result<i64, String> {
    if !mailbox.live() {
        return mailbox.demo.save_draft(draft);
    }
    let engine = mailbox.live.engine.clone();
    tauri::async_runtime::spawn_blocking(move || {
        save_live(&engine, &SAVED_FILES, &draft, read_local)
    })
    .await
    .map_err(text)?
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
    SAVED_FILES.forget(id);
    Ok(())
}

/// Opens a link from an email in the system browser or mail handler.
#[tauri::command]
pub fn open_external(url: String) -> Result<(), String> {
    let parsed = url::Url::parse(&url).map_err(|_| "That link isn't valid.".to_owned())?;
    if REFUSED_SCHEMES.contains(&parsed.scheme()) {
        return Err(format!("spark-rs doesn't open {} links.", parsed.scheme()));
    }
    open::that_detached(parsed.as_str()).map_err(text)
}

/// Links that never open, as in Spark (`lib/links.ts` asks before any other
/// scheme that isn't the web's, mail's or a phone's), plus Windows handlers
/// attackers have used to run code from a click.
const REFUSED_SCHEMES: &[&str] = &[
    "javascript",
    "data",
    "vbscript",
    "file",
    "sftp",
    "about",
    "blob",
    "ms-msdt",
    "search-ms",
    "search",
    "ms-officecmd",
];

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::path::Path;
    use std::sync::Arc;

    use super::*;
    use spark_store::NewAccount;

    #[test]
    fn script_file_and_attack_links_never_open() {
        for url in [
            "javascript:alert(1)",
            "file:///C:/Windows/System32/calc.exe",
            "data:text/html,<b>x</b>",
            "ms-msdt:/id PCWDiagnostic",
            "search-ms:query=x",
        ] {
            let err = open_external(url.into()).unwrap_err();
            assert!(err.contains("doesn't open"), "{url}: {err}");
        }
        assert_eq!(
            open_external("not a url".into()).unwrap_err(),
            "That link isn't valid."
        );
    }

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

    struct NoSecrets;

    impl spark_mail::Secrets for NoSecrets {
        fn get(&self, _: &str) -> Result<Option<String>, String> {
            Ok(None)
        }
        fn set(&self, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
    }

    /// An engine with an account and no network.
    fn engine() -> (spark_mail::Engine, Account) {
        let store = Store::open_in_memory().unwrap();
        let account = draft_account(&store);
        let engine = spark_mail::Engine::new(Arc::new(Mutex::new(store)), Arc::new(NoSecrets));
        (engine, account)
    }

    /// Writes a file to attach, last modified at `modified`.
    fn write_file(path: &Path, data: &[u8], modified: SystemTime) {
        std::fs::write(path, data).unwrap();
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(modified).unwrap();
    }

    fn ago(secs: u64) -> SystemTime {
        SystemTime::now() - Duration::from_secs(secs)
    }

    /// The draft's files for the outbox, read now.
    fn copies(draft: &Draft, store: &Store) -> Result<Vec<OutboxFile>, String> {
        let read = attached(draft)
            .iter()
            .map(|f| std::fs::read(f.path))
            .collect();
        outbox_files(draft, read, store)
    }

    fn bytes(files: &[OutboxFile]) -> Vec<&[u8]> {
        files.iter().map(|f| &f.data[..]).collect()
    }

    #[tokio::test]
    async fn a_recovered_draft_can_save_and_send_after_its_original_file_is_deleted() {
        let (engine, account) = engine();
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
        let queued = queue_live(&engine, &draft).unwrap();
        let files = engine.store().outbox_files(queued).unwrap();
        assert_eq!(bytes(&files), [b"saved attachment"]);
        std::fs::remove_file(&path).unwrap();
        let result = engine.send_due(account.id).await.unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].result.is_err());
        assert!(engine.store().outbox_files(queued).unwrap().is_empty());
        let id = engine.store().messages().unwrap()[0].id;
        let recovered = spark_mail::drafts::open(&engine.store(), id)
            .unwrap()
            .unwrap();
        assert_eq!(copies(&recovered, &engine.store()).unwrap(), files);
        let saved = save_live(&engine, &SavedFiles::default(), &recovered, read_local).unwrap();
        assert_eq!(saved, id);
        assert_eq!(engine.store().draft_files(id).unwrap(), files);
        let retry = queue_live(&engine, &recovered).unwrap();
        assert_eq!(engine.store().outbox_files(retry).unwrap(), files);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn saved_files_match_the_original_attachment_after_reordering() {
        let (engine, account) = engine();
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
        let id = save_live(&engine, &SavedFiles::default(), &draft, read_local).unwrap();
        let files = engine.store().draft_files(id).unwrap();
        assert_eq!(bytes(&files), [b"one", b"two"]);
        std::fs::remove_file(&first).unwrap();
        std::fs::remove_file(&second).unwrap();
        let store = engine.store();
        let mut opened = spark_mail::drafts::open(&store, id).unwrap().unwrap();
        opened.attachments.remove(0);
        assert_eq!(copies(&opened, &store).unwrap(), vec![files[1].clone()]);
        // The same name and size at a different path is a new attachment.
        opened.attachments[1] = file_attachment(&dir.join("elsewhere/second.txt"), 3);
        assert!(copies(&opened, &store).is_err());
        drop(store);
        assert!(save_live(&engine, &SavedFiles::default(), &opened, read_local).is_err());
        // When its original exists, use its current bytes rather than an old copy.
        opened.attachments[1] = file_attachment(&second, 3);
        std::fs::write(&second, b"new").unwrap();
        assert_eq!(bytes(&copies(&opened, &engine.store()).unwrap()), [b"new"]);
        save_live(&engine, &SavedFiles::default(), &opened, read_local).unwrap();
        assert_eq!(bytes(&engine.store().draft_files(id).unwrap()), [b"new"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn text_only_autosaves_read_their_files_once() {
        let (engine, account) = engine();
        let dir = attachment_dir("autosaves");
        let (a, b) = (dir.join("a.bin"), dir.join("b.bin"));
        let size = 12 << 20;
        write_file(&a, &vec![1; size], ago(3_600));
        write_file(&b, &vec![2; size], ago(3_600));
        let mut draft = Draft {
            account: account.id,
            subject: "Plan".into(),
            attachments: vec![
                file_attachment(&a, size as u64),
                file_attachment(&b, size as u64),
            ],
            ..Default::default()
        };
        let saved = SavedFiles::default();
        let reads = Cell::new(0);
        let read = |path: &str, name: &str| {
            reads.set(reads.get() + 1);
            read_local(path, name)
        };
        let id = save_live(&engine, &saved, &draft, read).unwrap();
        assert_eq!(reads.get(), 2);
        draft.draft_id = Some(id);
        for i in 0..20 {
            draft.body = format!("Draft {i}");
            draft.subject = format!("Plan {}", i / 2);
            assert_eq!(save_live(&engine, &saved, &draft, read).unwrap(), id);
        }
        assert_eq!(reads.get(), 2);
        // Each kept the copies saved, so the store didn't read them either.
        let store = engine.store();
        let kept = store.saved_files(id).unwrap();
        let snapshot = saved.current(&store, id, &kept).unwrap();
        let found: Vec<_> = attached(&draft)
            .iter()
            .map(|f| Source::now(f.path))
            .collect();
        let unread: Vec<_> = found.iter().map(|_| None).collect();
        let picked = pick(&store, &draft, &kept, snapshot.as_ref(), &found, &unread).unwrap();
        let Picked::All(picks) = picked else {
            panic!("it reads files again");
        };
        assert!(
            picks
                .iter()
                .enumerate()
                .all(|(i, p)| matches!(p, Pick::Saved(at, Some(_)) if *at == i))
        );
        let opened = spark_mail::drafts::open(&store, id).unwrap().unwrap();
        assert_eq!((&*opened.subject, &*opened.body), ("Plan 9", "Draft 19"));
        let files = store.draft_files(id).unwrap();
        assert_eq!(bytes(&files), [&vec![1; size][..], &vec![2; size][..]]);
        drop(store);

        // After a restart, the first save reads them again.
        let restarted = SavedFiles::default();
        save_live(&engine, &restarted, &draft, read).unwrap();
        save_live(&engine, &restarted, &draft, read).unwrap();
        assert_eq!(reads.get(), 4);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_file_replaced_or_written_since_it_was_saved_is_read_again() {
        let (engine, account) = engine();
        let dir = attachment_dir("replaced");
        let path = dir.join("report.pdf");
        write_file(&path, b"version one", ago(3_600));
        let mut draft = Draft {
            account: account.id,
            attachments: vec![file_attachment(&path, 11)],
            ..Default::default()
        };
        let saved = SavedFiles::default();
        let reads = Cell::new(0);
        let read = |path: &str, name: &str| {
            reads.set(reads.get() + 1);
            read_local(path, name)
        };
        let id = save_live(&engine, &saved, &draft, read).unwrap();
        draft.draft_id = Some(id);
        save_live(&engine, &saved, &draft, read).unwrap();
        assert_eq!(reads.get(), 1);

        // The same name and size, other bytes and modified time.
        write_file(&path, b"version two", ago(1_800));
        draft.body = "Updated".into();
        save_live(&engine, &saved, &draft, read).unwrap();
        assert_eq!(reads.get(), 2);
        let files = engine.store().draft_files(id).unwrap();
        assert_eq!(bytes(&files), [b"version two"]);

        // Modified as it's read, it isn't trusted unchanged by the next save.
        write_file(
            &path,
            b"version 3!!",
            SystemTime::now() + Duration::from_secs(60),
        );
        save_live(&engine, &saved, &draft, read).unwrap();
        save_live(&engine, &saved, &draft, read).unwrap();
        assert_eq!(reads.get(), 4);
        let files = engine.store().draft_files(id).unwrap();
        assert_eq!(bytes(&files), [b"version 3!!"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reordered_and_removed_files_keep_their_saved_copies() {
        let (engine, account) = engine();
        let dir = attachment_dir("reordered");
        let [a, b, c] = ["a.txt", "b.txt", "c.txt"].map(|name| dir.join(name));
        write_file(&a, b"one", ago(3_600));
        write_file(&b, b"two", ago(3_600));
        write_file(&c, b"three", ago(3_600));
        let mut draft = Draft {
            account: account.id,
            attachments: vec![
                file_attachment(&a, 3),
                file_attachment(&b, 3),
                file_attachment(&c, 5),
            ],
            ..Default::default()
        };
        let saved = SavedFiles::default();
        let reads = Cell::new(0);
        let read = |path: &str, name: &str| {
            reads.set(reads.get() + 1);
            read_local(path, name)
        };
        let id = save_live(&engine, &saved, &draft, read).unwrap();
        draft.draft_id = Some(id);
        assert_eq!(reads.get(), 3);

        draft.attachments = vec![file_attachment(&c, 5), file_attachment(&a, 3)];
        save_live(&engine, &saved, &draft, read).unwrap();
        assert_eq!(reads.get(), 3);
        let files = engine.store().draft_files(id).unwrap();
        assert_eq!(bytes(&files), [&b"three"[..], b"one"]);

        // B was removed from the draft: back, it's read again.
        draft.attachments.insert(1, file_attachment(&b, 3));
        save_live(&engine, &saved, &draft, read).unwrap();
        assert_eq!(reads.get(), 4);
        let files = engine.store().draft_files(id).unwrap();
        assert_eq!(bytes(&files), [&b"three"[..], b"two", b"one"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_original_keeps_the_copy_saved_with_the_draft() {
        let (engine, account) = engine();
        let dir = attachment_dir("missing");
        let (a, b) = (dir.join("a.txt"), dir.join("b.txt"));
        write_file(&a, b"one", ago(3_600));
        write_file(&b, b"two", ago(3_600));
        let mut draft = Draft {
            account: account.id,
            to: "ana@example.test".into(),
            attachments: vec![file_attachment(&a, 3), file_attachment(&b, 3)],
            ..Default::default()
        };
        let saved = SavedFiles::default();
        let reads = Cell::new(0);
        let read = |path: &str, name: &str| -> io::Result<LocalCopy> {
            let copy = read_local(path, name)?;
            reads.set(reads.get() + 1);
            Ok(copy)
        };
        let id = save_live(&engine, &saved, &draft, read).unwrap();
        draft.draft_id = Some(id);
        std::fs::remove_file(&a).unwrap();
        for body in ["Edited", "Edited again"] {
            draft.body = body.into();
            assert_eq!(save_live(&engine, &saved, &draft, read).unwrap(), id);
        }
        assert_eq!(reads.get(), 2);
        let files = engine.store().draft_files(id).unwrap();
        assert_eq!(bytes(&files), [b"one", b"two"]);
        let queued = queue_live(&engine, &draft).unwrap();
        assert_eq!(engine.store().outbox_files(queued).unwrap(), files);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_draft_moved_to_another_account_keeps_its_files_unread() {
        let (engine, account) = engine();
        let other = engine
            .store()
            .add_account(&NewAccount {
                kind: AccountKind::Imap,
                email: "me@example.org",
                name: "Me",
                label: "Other",
                config: "{}",
            })
            .unwrap();
        let dir = attachment_dir("moved");
        let path = dir.join("plan.txt");
        write_file(&path, b"the plan", ago(3_600));
        let mut draft = Draft {
            account: account.id,
            attachments: vec![file_attachment(&path, 8)],
            ..Default::default()
        };
        let saved = SavedFiles::default();
        let reads = Cell::new(0);
        let read = |path: &str, name: &str| {
            reads.set(reads.get() + 1);
            read_local(path, name)
        };
        draft.draft_id = Some(save_live(&engine, &saved, &draft, read).unwrap());
        draft.account = other.id;
        let moved = save_live(&engine, &saved, &draft, read).unwrap();
        draft.draft_id = Some(moved);
        save_live(&engine, &saved, &draft, read).unwrap();
        assert_eq!(reads.get(), 1);
        let store = engine.store();
        assert_eq!(store.messages().unwrap().len(), 1);
        assert_eq!(bytes(&store.draft_files(moved).unwrap()), [b"the plan"]);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn saves_and_sends_at_once_queue_it_once_with_its_exact_files() {
        let (engine, account) = engine();
        let dir = attachment_dir("concurrent");
        let (a, b) = (dir.join("a.bin"), dir.join("b.bin"));
        write_file(&a, &vec![1; 1 << 20], ago(3_600));
        write_file(&b, &vec![2; 1 << 20], ago(3_600));
        let mut draft = Draft {
            account: account.id,
            to: "ana@example.test".into(),
            attachments: vec![file_attachment(&a, 1 << 20), file_attachment(&b, 1 << 20)],
            ..Default::default()
        };
        let saved = SavedFiles::default();
        let id = save_live(&engine, &saved, &draft, read_local).unwrap();
        draft.draft_id = Some(id);
        let expected = engine.store().draft_files(id).unwrap();
        let (sends, saves) = std::thread::scope(|s| {
            let sends: Vec<_> = (0..2)
                .map(|_| s.spawn(|| queue_live(&engine, &draft)))
                .collect();
            let saves: Vec<_> = (0..4)
                .map(|i| {
                    let edited = Draft {
                        body: format!("Edit {i}"),
                        ..draft.clone()
                    };
                    let (engine, saved) = (&engine, &saved);
                    s.spawn(move || save_live(engine, saved, &edited, read_local))
                })
                .collect();
            let joined = |handles: Vec<std::thread::ScopedJoinHandle<_>>| {
                handles
                    .into_iter()
                    .map(|h| h.join().unwrap())
                    .collect::<Vec<Result<i64, String>>>()
            };
            (joined(sends), joined(saves))
        });
        let queued: Vec<i64> = sends.iter().filter_map(|r| r.clone().ok()).collect();
        assert_eq!(queued.len(), 1, "{sends:?}");
        assert!(sends.contains(&Err("This draft is already on its way.".into())));
        assert!(saves.iter().all(|r| r == &Ok(id)), "{saves:?}");
        let store = engine.store();
        assert!(store.draft_held(id).unwrap());
        assert_eq!(store.outbox_files(queued[0]).unwrap(), expected);
        assert_eq!(store.draft_files(id).unwrap(), expected);
        drop(store);
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
