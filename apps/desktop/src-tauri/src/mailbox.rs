//! What the UI talks to: real accounts once one is added, the demo mailbox
//! until then. Types here are the UI's shapes (`src/types.ts` mirrors them).

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Serialize;
use spark_mail::attachment::{AttachmentData, mime_for};
use spark_mail::draft::{self, Draft, OutgoingAttachment};
use spark_mail::engine::{folder_has_older, gmail_servers, inbox_has_older};
use spark_mail::imap::Server;
use spark_store::{
    Account, AccountKind, Category, Folder, FolderRole, MessageState, OutboxFile, Patch, now_ms,
};
use tauri::{AppHandle, Emitter, State, Window};

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
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageContent {
    text: Vec<String>,
    /// The email's own HTML, untrusted: the UI renders it in a sandboxed frame.
    html: Option<String>,
    to: Vec<String>,
    cc: Vec<String>,
    attachments: Vec<AttachmentView>,
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
    fn live(&self) -> bool {
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
        to: m.to,
        timestamp: m.date,
        subject: m.subject,
        snippet: m.snippet,
        body: None,
        attachment: m.attachment,
        folders: m.folders,
        folder: m.folder,
        conversation: Some(m.conversation),
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
            attachments: m
                .attachment
                .into_iter()
                .map(|name| AttachmentView {
                    mime: mime_for(&name).into(),
                    name,
                    size: 0,
                })
                .collect(),
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
        attachments: content
            .attachments
            .into_iter()
            .map(|a| AttachmentView {
                name: a.name,
                mime: a.mime,
                size: a.size,
            })
            .collect(),
    })
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
        // The demo pretends.
        return Ok(mailbox.demo.send(&draft));
    }
    // Catch typos and oversized attachments now rather than after the undo window.
    for field in [&draft.to, &draft.cc, &draft.bcc] {
        draft::recipients(field).map_err(text)?;
    }
    draft::check_attachment_size(&draft.attachments).map_err(text)?;
    // Copies of local files go into the outbox, so a later send doesn't
    // depend on the originals.
    let queued = local_files(&draft)?;
    let json = serde_json::to_string(&draft).map_err(text)?;
    let mut store = mailbox.live.engine.store();
    let id = store
        .queue_send(draft.account, &json, draft.send_at, &queued)
        .map_err(text)?;
    // The saved draft it came from waits with it, out of the lists.
    if let Some(saved) = draft.draft_id {
        store.hold_draft_for_send(id, saved).map_err(text)?;
    }
    drop(store);
    if draft.send_at <= now_ms() {
        mailbox.live.nudge(draft.account);
    }
    Ok(id)
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

/// Copies of a draft's files from this computer, in order.
fn local_files(draft: &Draft) -> Result<Vec<OutboxFile>, String> {
    let mut files = Vec::new();
    for a in &draft.attachments {
        if let OutgoingAttachment::File { path, name, .. } = a {
            let data = std::fs::read(path).map_err(|e| format!("Couldn't read {name}: {e}"))?;
            files.push(OutboxFile {
                name: name.clone(),
                mime: mime_for(name).into(),
                data,
            });
        }
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
    let files = local_files(&draft)?;
    mailbox.live.engine.save_draft(&draft, &files).map_err(text)
}

/// A draft from Drafts, for the composer.
#[tauri::command]
pub fn open_draft(mailbox: State<'_, Mailbox>, id: i64) -> Result<Draft, String> {
    let opened = if mailbox.live() {
        mailbox.live.engine.open_draft(id).map_err(text)?
    } else {
        mailbox.demo.open_draft(id)
    };
    opened.ok_or_else(|| "That draft is gone.".to_owned())
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
            folders: vec![4],
            folder: None,
            to: vec!["Me <me@x.com>".into()],
            conversation: 2,
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
        assert_eq!(json["folders"], serde_json::json!([4]));
        assert!(json.get("folder").is_none());
        assert_eq!(json["to"], serde_json::json!(["Me <me@x.com>"]));
    }
}
