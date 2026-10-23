//! Microsoft Graph for Microsoft 365 mailboxes: the inbox through delta
//! queries, other folders' newest messages, flag and move changes, folder
//! management, and sending.
//!
//! Every request asks for immutable ids, so a message keeps its id when it
//! moves to Archive or Deleted Items and back.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use reqwest::{Method, RequestBuilder, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use spark_store::{
    Added, Attachment, Category, Folder, FolderRole, Incoming, Pending, Place, Pushed,
    RemoteFolder, Store,
};

use crate::attachment::{AttachmentData, mime_for};
use crate::draft::{Draft, recipients};
use crate::parse::{automated_sender, bare_id, html_to_text, snippet};
use crate::{Error, Result};

pub const GRAPH: &str = "https://graph.microsoft.com/v1.0";
/// How far back the first sync of a Microsoft account reaches.
pub const FIRST_SYNC_DAYS: i64 = 30;
const HOST: &str = "graph.microsoft.com";
/// Graph takes attachments smaller than this in one request; bigger ones go
/// up through an upload session.
const INLINE_ATTACHMENT_MAX: usize = 3 * 1024 * 1024;
/// Upload session chunks must be multiples of 320 KiB.
const UPLOAD_CHUNK: usize = 10 * 320 * 1024;
/// A folder shows its newest this many messages.
pub const FOLDER_LIMIT: usize = 200;
/// More new messages than this in a folder are fetched in one listing
/// rather than one by one.
const FETCH_ONE_BY_ONE: usize = 10;
/// Well-known folders: their role, or `None` for ones left out of the list.
/// Folders inside the inbox are listed at the top.
const WELL_KNOWN: [(&str, Option<FolderRole>); 8] = [
    ("inbox", None),
    ("archive", Some(FolderRole::Archive)),
    ("sentitems", Some(FolderRole::Sent)),
    ("drafts", Some(FolderRole::Drafts)),
    ("deleteditems", Some(FolderRole::Trash)),
    ("junkemail", Some(FolderRole::Spam)),
    ("outbox", None),
    ("conversationhistory", None),
];
const FOLDER_SELECT: &str = "id,displayName,childFolderCount";

const SELECT: &str = "subject,from,toRecipients,ccRecipients,bccRecipients,receivedDateTime,\
     isRead,flag,bodyPreview,body,hasAttachments,internetMessageId,conversationId,\
     inferenceClassification";

/// One HTTP client for the process (connection reuse).
pub(crate) fn http() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(Duration::from_secs(90))
                .user_agent(concat!("spark-rs/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("the HTTP client builds")
        })
        .clone()
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncState {
    pub delta_link: Option<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Ids of the messages stored for the first time.
    pub new: Vec<i64>,
    pub gone: usize,
    pub first: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Me {
    pub display_name: Option<String>,
    pub mail: Option<String>,
    pub user_principal_name: Option<String>,
}

impl Me {
    pub fn address(&self) -> Option<&str> {
        self.mail
            .as_deref()
            .or(self.user_principal_name.as_deref())
            .filter(|a| a.contains('@'))
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphMessage {
    pub id: String,
    #[serde(rename = "@removed")]
    pub removed: Option<Value>,
    pub subject: Option<String>,
    pub from: Option<GraphRecipient>,
    #[serde(default)]
    pub to_recipients: Vec<GraphRecipient>,
    #[serde(default)]
    pub cc_recipients: Vec<GraphRecipient>,
    #[serde(default)]
    pub bcc_recipients: Vec<GraphRecipient>,
    pub received_date_time: Option<String>,
    pub is_read: Option<bool>,
    pub flag: Option<GraphFlag>,
    pub body_preview: Option<String>,
    pub body: Option<GraphBody>,
    pub has_attachments: Option<bool>,
    pub internet_message_id: Option<String>,
    pub conversation_id: Option<String>,
    pub inference_classification: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphRecipient {
    pub email_address: GraphAddress,
}

#[derive(Debug, Default, Deserialize)]
pub struct GraphAddress {
    pub name: Option<String>,
    pub address: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphFlag {
    pub flag_status: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphBody {
    pub content_type: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphFolder {
    pub id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub child_folder_count: u32,
}

#[derive(Debug, Deserialize)]
struct Page<T> {
    #[serde(default = "Vec::new")]
    value: Vec<T>,
    #[serde(rename = "@odata.nextLink")]
    next_link: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeltaPage {
    #[serde(default)]
    value: Vec<GraphMessage>,
    #[serde(rename = "@odata.nextLink")]
    next_link: Option<String>,
    #[serde(rename = "@odata.deltaLink")]
    delta_link: Option<String>,
}

pub struct Graph {
    token: String,
    base: String,
}

impl Graph {
    pub fn new(access_token: &str) -> Self {
        Self::with_base(GRAPH, access_token)
    }

    /// For tests against a local stand-in.
    pub fn with_base(base: &str, access_token: &str) -> Self {
        Self {
            token: access_token.to_owned(),
            base: base.trim_end_matches('/').to_owned(),
        }
    }

    fn request(&self, method: Method, url: &str) -> RequestBuilder {
        let url = if url.starts_with("http") {
            url.to_owned()
        } else {
            format!("{}{url}", self.base)
        };
        http()
            .request(method, url)
            .bearer_auth(&self.token)
            .header("Prefer", "IdType=\"ImmutableId\"")
    }

    async fn send(&self, request: RequestBuilder) -> Result<reqwest::Response> {
        let response = request.send().await.map_err(|e| Error::network(HOST, e))?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let detail = response
            .json::<Value>()
            .await
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
            .unwrap_or_else(|| status.to_string());
        Err(match status {
            StatusCode::UNAUTHORIZED => Error::Auth("Microsoft needs you to sign in again.".into()),
            StatusCode::FORBIDDEN => Error::Auth(format!(
                "Microsoft refused access ({detail}). Your organization may need to approve the app."
            )),
            StatusCode::NOT_FOUND | StatusCode::GONE => Error::Gone,
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
                Error::network(HOST, "Microsoft is busy; trying again shortly")
            }
            _ => Error::Server(format!("Microsoft Graph: {detail}")),
        })
    }

    async fn json<T: for<'de> Deserialize<'de>>(&self, request: RequestBuilder) -> Result<T> {
        self.send(request)
            .await?
            .json()
            .await
            .map_err(|e| Error::Server(format!("Microsoft Graph sent an unexpected reply: {e}")))
    }

    pub async fn me(&self) -> Result<Me> {
        self.json(self.request(
            Method::GET,
            "/me?$select=displayName,mail,userPrincipalName",
        ))
        .await
    }

    /// Brings the inbox into the store through a delta query.
    pub async fn sync_inbox(
        &self,
        store: &Mutex<Store>,
        account_id: i64,
        state: &mut SyncState,
    ) -> Result<SyncReport> {
        let first = state.delta_link.is_none();
        let mut report = SyncReport {
            first,
            ..Default::default()
        };
        let mut url = match &state.delta_link {
            Some(link) => link.clone(),
            None => first_delta_url(spark_store::now_ms()),
        };
        loop {
            let request = self
                .request(Method::GET, &url)
                .header("Prefer", "odata.maxpagesize=50")
                .header("Prefer", "outlook.body-content-type=\"html\"");
            let page: DeltaPage = match self.json(request).await {
                // The delta token expired: start over.
                Err(Error::Gone) if !first => {
                    state.delta_link = None;
                    return Box::pin(self.sync_inbox(store, account_id, state)).await;
                }
                other => other?,
            };
            for m in page.value {
                if m.removed.is_some() {
                    report.gone += lock(store).observe_gone_remote(account_id, &m.id)?;
                    continue;
                }
                // Updates without flags tell us nothing we use.
                if m.is_read.is_none() {
                    continue;
                }
                let mut incoming = to_incoming(account_id, &m);
                if m.has_attachments == Some(true)
                    && !lock(store).has_remote_id(account_id, &m.id)?
                {
                    incoming.attachments = self.attachments(&m.id).await.unwrap_or_default();
                }
                if let spark_store::Added::New(id) = lock(store).add_incoming(&incoming, !first)? {
                    report.new.push(id);
                }
            }
            match (page.next_link, page.delta_link) {
                (Some(next), _) => url = next,
                (None, Some(delta)) => {
                    state.delta_link = Some(delta);
                    return Ok(report);
                }
                (None, None) => {
                    return Err(Error::Server(
                        "Microsoft Graph ended the inbox listing early.".into(),
                    ));
                }
            }
        }
    }

    /// The mailbox's folders, nested ones after their parents. Well-known
    /// folders get their role; the inbox, Outbox and Conversation History
    /// aren't listed (folders inside the inbox are, at the top).
    pub async fn list_folders(&self) -> Result<Vec<RemoteFolder>> {
        let mut well_known: HashMap<String, Option<FolderRole>> = HashMap::new();
        let mut inbox = None;
        for (name, role) in WELL_KNOWN {
            match self
                .json::<GraphFolder>(
                    self.request(Method::GET, &format!("/me/mailFolders/{name}?$select=id")),
                )
                .await
            {
                Ok(f) => {
                    if name == "inbox" {
                        inbox = Some(f.id.clone());
                    }
                    well_known.insert(f.id, role);
                }
                // Not every mailbox has every folder (e.g. no archive).
                Err(Error::Gone) => {}
                Err(e) => return Err(e),
            }
        }
        let mut folders = Vec::new();
        let mut queue = VecDeque::from([(
            format!("/me/mailFolders?$top=250&$select={FOLDER_SELECT}"),
            None::<String>,
        )]);
        while let Some((first, parent)) = queue.pop_front() {
            let mut next = Some(first);
            while let Some(url) = next {
                let page: Page<GraphFolder> = self.json(self.request(Method::GET, &url)).await?;
                for f in page.value {
                    let listed = match well_known.get(&f.id) {
                        Some(None) => Some(&f.id) == inbox.as_ref(),
                        _ => true,
                    };
                    // Children of the inbox go to the top; of hidden ones, nowhere.
                    if f.child_folder_count > 0 && (listed || Some(&f.id) == inbox.as_ref()) {
                        let child_parent = (Some(&f.id) != inbox.as_ref()).then(|| f.id.clone());
                        queue.push_back((
                            format!(
                                "/me/mailFolders/{}/childFolders?$top=250&$select={FOLDER_SELECT}",
                                path(&f.id)
                            ),
                            child_parent,
                        ));
                    }
                    if Some(&f.id) == inbox.as_ref() || !listed {
                        continue;
                    }
                    folders.push(RemoteFolder {
                        role: well_known.get(&f.id).copied().flatten(),
                        remote_id: f.id,
                        parent: parent.clone(),
                        name: f.display_name,
                        selectable: true,
                    });
                }
                next = page.next_link;
            }
            if folders.len() > 2_000 {
                break;
            }
        }
        Ok(folders)
    }

    /// Brings a folder's newest messages (up to `FOLDER_LIMIT`) into the
    /// store and makes them its list. Returns how many came or went, or
    /// `None` if the folder is gone.
    pub async fn sync_folder(
        &self,
        store: &Mutex<Store>,
        folder: &Folder,
    ) -> Result<Option<usize>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Listed {
            id: String,
            is_read: Option<bool>,
            flag: Option<GraphFlag>,
        }
        let account_id = folder.account_id;
        let messages = format!("/me/mailFolders/{}/messages", path(&folder.remote_id));
        let listing: Page<Listed> = match self
            .json(self.request(
                Method::GET,
                &format!(
                    "{messages}?$top={FOLDER_LIMIT}&$orderby=receivedDateTime+desc&$select=id,isRead,flag"
                ),
            ))
            .await
        {
            Err(Error::Gone) => return Ok(None),
            other => other?,
        };
        let (before, known): (HashSet<i64>, HashSet<String>) = {
            let store = lock(store);
            let mut known = HashSet::new();
            for m in &listing.value {
                if store.has_remote_id(account_id, &m.id)? {
                    known.insert(m.id.clone());
                }
            }
            (store.folder_member_ids(folder.id)?, known)
        };

        // The new ones in full: one by one, or the whole listing again.
        let unknown: Vec<&str> = listing
            .value
            .iter()
            .map(|m| m.id.as_str())
            .filter(|id| !known.contains(*id))
            .collect();
        let mut full: HashMap<String, GraphMessage> = HashMap::new();
        if unknown.len() > FETCH_ONE_BY_ONE {
            let mut next = Some(format!(
                "{messages}?$top=50&$orderby=receivedDateTime+desc&$select={SELECT}"
            ));
            while let Some(url) = next.take() {
                let page: Page<GraphMessage> = self
                    .json(
                        self.request(Method::GET, &url)
                            .header("Prefer", "outlook.body-content-type=\"html\""),
                    )
                    .await?;
                full.extend(page.value.into_iter().map(|m| (m.id.clone(), m)));
                if full.len() < FOLDER_LIMIT {
                    next = page.next_link;
                }
            }
        } else {
            for id in &unknown {
                match self
                    .json::<GraphMessage>(
                        self.request(
                            Method::GET,
                            &format!("/me/messages/{}?$select={SELECT}", path(id)),
                        )
                        .header("Prefer", "outlook.body-content-type=\"html\""),
                    )
                    .await
                {
                    Ok(m) => {
                        full.insert(m.id.clone(), m);
                    }
                    // Moved or deleted since the listing.
                    Err(Error::Gone) => {}
                    Err(e) => return Err(e),
                }
            }
        }

        let mut changed = 0;
        let mut members = Vec::new();
        for listed in &listing.value {
            let unread = !listed.is_read.unwrap_or(false);
            let flagged = listed
                .flag
                .as_ref()
                .is_some_and(|f| f.flag_status == "flagged");
            let incoming = if known.contains(&listed.id) {
                Incoming {
                    account_id,
                    remote_id: Some(listed.id.clone()),
                    unread,
                    flagged,
                    ..Default::default()
                }
            } else if let Some(m) = full.get(&listed.id) {
                let mut incoming = to_incoming(account_id, m);
                if m.has_attachments == Some(true) {
                    incoming.attachments = self.attachments(&m.id).await.unwrap_or_default();
                }
                incoming
            } else {
                continue;
            };
            let id = match lock(store).add_folder_message(folder.id, &incoming)? {
                Added::New(id) | Added::Known(id) => id,
            };
            if !before.contains(&id) {
                changed += 1;
            }
            members.push((id, None));
        }
        changed += lock(store).set_folder_members(folder.id, &members)?;
        Ok(Some(changed))
    }

    /// Creates a folder, in `parent` (a folder id) or at the top; returns its id.
    pub async fn create_folder(&self, parent: Option<&str>, name: &str) -> Result<String> {
        let url = match parent {
            Some(parent) => format!("/me/mailFolders/{}/childFolders", path(parent)),
            None => "/me/mailFolders".to_owned(),
        };
        let created: GraphFolder = self
            .json(
                self.request(Method::POST, &url)
                    .json(&json!({ "displayName": name })),
            )
            .await?;
        Ok(created.id)
    }

    pub async fn rename_folder(&self, id: &str, name: &str) -> Result<()> {
        self.send(
            self.request(Method::PATCH, &format!("/me/mailFolders/{}", path(id)))
                .json(&json!({ "displayName": name })),
        )
        .await?;
        Ok(())
    }

    /// Deletes a folder with its subfolders (Outlook keeps them in Deleted Items).
    pub async fn delete_folder(&self, id: &str) -> Result<()> {
        self.send(self.request(Method::DELETE, &format!("/me/mailFolders/{}", path(id))))
            .await?;
        Ok(())
    }

    /// A message's attachments, not counting inline images.
    pub async fn attachments(&self, id: &str) -> Result<Vec<Attachment>> {
        #[derive(Deserialize)]
        struct Page {
            value: Vec<Listed>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Listed {
            id: Option<String>,
            name: Option<String>,
            content_type: Option<String>,
            size: Option<u64>,
            is_inline: Option<bool>,
        }
        let page: Page = self
            .json(self.request(
                Method::GET,
                &format!(
                    "/me/messages/{}/attachments?$select=id,name,contentType,size,isInline",
                    path(id)
                ),
            ))
            .await?;
        Ok(page
            .value
            .into_iter()
            .filter(|a| a.is_inline != Some(true))
            .filter_map(|a| {
                Some(Attachment {
                    name: a.name?,
                    mime: a.content_type.unwrap_or_default(),
                    size: a.size.unwrap_or(0),
                    remote_id: a.id,
                })
            })
            .collect())
    }

    /// Downloads `wanted`, attachment `index` of message `id` as the store
    /// lists it: by its attachment id, else by position and name.
    pub async fn attachment(
        &self,
        id: &str,
        index: usize,
        wanted: &Attachment,
    ) -> Result<AttachmentData> {
        let attachment_id = match &wanted.remote_id {
            Some(attachment_id) => attachment_id.clone(),
            None => {
                let listed = self.attachments(id).await?;
                listed
                    .get(index)
                    .filter(|a| a.name == wanted.name)
                    .or_else(|| listed.iter().find(|a| a.name == wanted.name))
                    .and_then(|a| a.remote_id.clone())
                    .ok_or_else(|| {
                        Error::Server(format!(
                            "{} is no longer attached to the email.",
                            wanted.name
                        ))
                    })?
            }
        };
        let response = self
            .send(self.request(
                Method::GET,
                &format!(
                    "/me/messages/{}/attachments/{}/$value",
                    path(id),
                    path(&attachment_id)
                ),
            ))
            .await?;
        let bytes = response
            .bytes()
            .await
            .map_err(|e| Error::network(HOST, e))?;
        Ok(AttachmentData {
            name: wanted.name.clone(),
            mime: if wanted.mime.is_empty() {
                mime_for(&wanted.name).to_owned()
            } else {
                wanted.mime.clone()
            },
            bytes: bytes.to_vec(),
        })
    }

    /// Sends local changes: read and flagged state, then the move for Done
    /// (Archive), Delete (Deleted Items), filing into a folder, or back to
    /// the inbox.
    pub async fn push(&self, store: &Mutex<Store>, pending: Vec<Pending>) -> Result<usize> {
        let mut pushed = 0;
        for p in pending {
            let Some(id) = p.remote_id.clone() else {
                lock(store).drop_pending(p.id)?;
                continue;
            };
            let result = async {
                if p.unread != p.remote_unread || p.pinned != p.remote_pinned {
                    let flag = if p.pinned { "flagged" } else { "notFlagged" };
                    let body = json!({ "isRead": !p.unread, "flag": { "flagStatus": flag } });
                    self.send(
                        self.request(Method::PATCH, &format!("/me/messages/{}", path(&id)))
                            .json(&body),
                    )
                    .await?;
                }
                if let Some(destination) = move_destination(&p) {
                    self.send(
                        self.request(Method::POST, &format!("/me/messages/{}/move", path(&id)))
                            .json(&json!({ "destinationId": destination })),
                    )
                    .await?;
                }
                Ok(())
            }
            .await;
            match result {
                Ok(()) => {
                    lock(store).mark_pushed(
                        &p,
                        Pushed {
                            mailbox: None,
                            uid: None,
                        },
                    )?;
                    pushed += 1;
                }
                Err(Error::Gone) => lock(store).drop_pending(p.id)?,
                Err(e) => return Err(e),
            }
        }
        Ok(pushed)
    }

    /// Sends a new message, or a reply threaded under `reply_to` (a Graph
    /// message id). Replies go through createReply so Outlook adds the
    /// quoted original and threading itself.
    pub async fn send_draft(
        &self,
        draft: &Draft,
        reply_to: Option<&str>,
        files: &[AttachmentData],
    ) -> Result<()> {
        let to = graph_recipients(&draft.to)?;
        let cc = graph_recipients(&draft.cc)?;
        let bcc = graph_recipients(&draft.bcc)?;
        if to.is_empty() && cc.is_empty() && bcc.is_empty() {
            return Err(Error::Config("Add at least one recipient.".into()));
        }
        let Some(original) = reply_to else {
            let message = json!({
                "subject": draft.subject.trim(),
                "body": { "contentType": "Text", "content": draft.body },
                "toRecipients": to, "ccRecipients": cc, "bccRecipients": bcc,
            });
            if files.is_empty() {
                self.send(
                    self.request(Method::POST, "/me/sendMail")
                        .json(&json!({ "message": message, "saveToSentItems": true })),
                )
                .await?;
                return Ok(());
            }
            // Attachments go on a draft, which is then sent.
            let created: GraphMessage = self
                .json(self.request(Method::POST, "/me/messages").json(&message))
                .await?;
            return self
                .attach_and_send(&format!("/me/messages/{}", path(&created.id)), files)
                .await;
        };

        let reply: GraphMessage = self
            .json(
                self.request(
                    Method::POST,
                    &format!("/me/messages/{}/createReply", path(original)),
                )
                .json(&json!({})),
            )
            .await?;
        let quoted = reply.body.map(|b| b.content).unwrap_or_default();
        let update = json!({
            "subject": draft.subject.trim(),
            "body": { "contentType": "HTML", "content": format!("{}{quoted}", text_to_html(&draft.body)) },
            "toRecipients": to, "ccRecipients": cc, "bccRecipients": bcc,
        });
        let draft_path = format!("/me/messages/{}", path(&reply.id));
        self.send(self.request(Method::PATCH, &draft_path).json(&update))
            .await?;
        self.attach_and_send(&draft_path, files).await
    }

    /// Adds `files` to a draft and sends it. If that fails, the draft is
    /// deleted so it doesn't linger in Drafts.
    async fn attach_and_send(&self, draft_path: &str, files: &[AttachmentData]) -> Result<()> {
        let result = async {
            for f in files {
                self.attach(draft_path, f).await?;
            }
            self.send(
                self.request(Method::POST, &format!("{draft_path}/send"))
                    .json(&json!({})),
            )
            .await?;
            Ok(())
        }
        .await;
        if result.is_err() {
            let _ = self.send(self.request(Method::DELETE, draft_path)).await;
        }
        result
    }

    async fn attach(&self, draft_path: &str, f: &AttachmentData) -> Result<()> {
        if f.bytes.len() < INLINE_ATTACHMENT_MAX {
            self.send(
                self.request(Method::POST, &format!("{draft_path}/attachments"))
                    .json(&json!({
                        "@odata.type": "#microsoft.graph.fileAttachment",
                        "name": f.name,
                        "contentType": f.mime,
                        "contentBytes": STANDARD.encode(&f.bytes),
                    })),
            )
            .await?;
            return Ok(());
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct UploadSession {
            upload_url: String,
        }
        let session: UploadSession = self
            .json(
                self.request(
                    Method::POST,
                    &format!("{draft_path}/attachments/createUploadSession"),
                )
                .json(&json!({
                    "AttachmentItem": {
                        "attachmentType": "file",
                        "name": f.name,
                        "size": f.bytes.len(),
                        "contentType": f.mime,
                    }
                })),
            )
            .await?;
        let total = f.bytes.len();
        for (i, chunk) in f.bytes.chunks(UPLOAD_CHUNK).enumerate() {
            let start = i * UPLOAD_CHUNK;
            let end = start + chunk.len() - 1;
            // The upload URL carries its own authorization.
            let response = http()
                .put(&session.upload_url)
                .header("Content-Range", format!("bytes {start}-{end}/{total}"))
                .body(chunk.to_vec())
                .send()
                .await
                .map_err(|e| Error::network(HOST, e))?;
            if !response.status().is_success() {
                return Err(Error::Server(format!(
                    "Microsoft didn't accept {} ({}).",
                    f.name,
                    response.status()
                )));
            }
        }
        Ok(())
    }
}

/// Where a pending change moves a message: a well-known folder name, or the
/// id of the folder it's filed into. `None` if it stays where it is.
fn move_destination(p: &Pending) -> Option<&str> {
    let destination = match p.place {
        Place::Inbox => "inbox",
        Place::Archive => "archive",
        Place::Trash => "deleteditems",
        // Filed mail goes to its home folder; without one it stays put.
        Place::Folder => {
            let (id, remote_id) = p.folder.as_ref()?;
            let there = p.remote_place == Place::Folder && p.remote_folder == Some(*id);
            return (!there).then_some(remote_id.as_str());
        }
    };
    (p.place != p.remote_place).then_some(destination)
}

/// Server drafts: they live in Drafts like Outlook's own.
impl Graph {
    /// Saves a draft: `update` (a draft's id) changed in place, or a new
    /// one in Drafts with `files` attached. Returns its id and Message-ID.
    /// A draft that's gone is made anew.
    pub async fn save_draft(
        &self,
        update: Option<&str>,
        draft: &Draft,
        files: &[AttachmentData],
    ) -> Result<(String, Option<String>)> {
        let message = draft_message(draft);
        if let Some(id) = update {
            match self
                .json::<GraphMessage>(
                    self.request(Method::PATCH, &format!("/me/messages/{}", path(id)))
                        .json(&message),
                )
                .await
            {
                Ok(m) => return Ok((m.id, m.internet_message_id.as_deref().map(bare_id))),
                Err(Error::Gone) => {}
                Err(e) => return Err(e),
            }
        }
        let created: GraphMessage = self
            .json(self.request(Method::POST, "/me/messages").json(&message))
            .await?;
        let draft_path = format!("/me/messages/{}", path(&created.id));
        for f in files {
            if let Err(e) = self.attach(&draft_path, f).await {
                let _ = self.send(self.request(Method::DELETE, &draft_path)).await;
                return Err(e);
            }
        }
        Ok((
            created.id,
            created.internet_message_id.as_deref().map(bare_id),
        ))
    }

    /// Deletes a draft for good, as Outlook does once it's sent; where
    /// that isn't offered, into Deleted Items.
    pub async fn delete_draft(&self, id: &str) -> Result<()> {
        let draft_path = format!("/me/messages/{}", path(id));
        let deleted = self
            .send(
                self.request(Method::POST, &format!("{draft_path}/permanentDelete"))
                    .json(&json!({})),
            )
            .await;
        let deleted = match deleted {
            Err(Error::Server(_)) => self.send(self.request(Method::DELETE, &draft_path)).await,
            other => other,
        };
        match deleted {
            Ok(_) | Err(Error::Gone) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// A draft as Graph takes it. Addresses that don't parse yet are left out.
pub fn draft_message(draft: &Draft) -> Value {
    let list = |field: &str| -> Vec<Value> {
        crate::drafts::recipients(field)
            .into_iter()
            .map(|m| {
                let mut address = json!({ "address": m.email.to_string() });
                if let Some(name) = m.name {
                    address["name"] = Value::String(name);
                }
                json!({ "emailAddress": address })
            })
            .collect()
    };
    json!({
        "subject": draft.subject.trim(),
        "body": { "contentType": "Text", "content": draft.body },
        "toRecipients": list(&draft.to),
        "ccRecipients": list(&draft.cc),
        "bccRecipients": list(&draft.bcc),
    })
}

/// The first delta request: the inbox's last `FIRST_SYNC_DAYS` days.
pub fn first_delta_url(now_ms: i64) -> String {
    let since = iso_date(now_ms - FIRST_SYNC_DAYS * 86_400_000);
    format!(
        "{GRAPH}/me/mailFolders/inbox/messages/delta?$select={SELECT}&$filter=receivedDateTime+ge+{since}"
    )
}

pub fn to_incoming(account_id: i64, m: &GraphMessage) -> Incoming {
    let (from_name, from_address) = m
        .from
        .as_ref()
        .map(|f| {
            let address = f.email_address.address.clone().unwrap_or_default();
            let name = f
                .email_address
                .name
                .clone()
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| address.clone());
            (name, address)
        })
        .unwrap_or_default();
    let (text, html) = match &m.body {
        Some(b) if b.content_type.eq_ignore_ascii_case("html") => {
            (html_to_text(&b.content), Some(b.content.clone()))
        }
        Some(b) => (b.content.clone(), None),
        None => (m.body_preview.clone().unwrap_or_default(), None),
    };
    let category = if automated_sender(&from_address) {
        Category::Notifications
    } else if m.inference_classification.as_deref() == Some("other") {
        Category::Newsletters
    } else {
        Category::People
    };
    Incoming {
        account_id,
        remote_id: Some(m.id.clone()),
        message_id: m.internet_message_id.as_deref().map(bare_id),
        thread_id: m.conversation_id.clone(),
        from_name,
        from_address,
        to: m.to_recipients.iter().filter_map(display).collect(),
        cc: m.cc_recipients.iter().filter_map(display).collect(),
        bcc: m.bcc_recipients.iter().filter_map(display).collect(),
        subject: m.subject.clone().unwrap_or_default().trim().to_owned(),
        snippet: snippet(m.body_preview.as_deref().unwrap_or(&text)),
        date: m
            .received_date_time
            .as_deref()
            .and_then(parse_iso_date)
            .unwrap_or_else(spark_store::now_ms),
        text,
        html,
        category,
        unread: !m.is_read.unwrap_or(false),
        flagged: m.flag.as_ref().is_some_and(|f| f.flag_status == "flagged"),
        ..Default::default()
    }
}

fn display(r: &GraphRecipient) -> Option<String> {
    let address = r.email_address.address.as_deref()?;
    Some(match r.email_address.name.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() && name != address => format!("{name} <{address}>"),
        _ => address.to_owned(),
    })
}

fn graph_recipients(field: &str) -> Result<Vec<Value>> {
    Ok(recipients(field)?
        .into_iter()
        .map(|m| {
            let mut address = json!({ "address": m.email.to_string() });
            if let Some(name) = m.name {
                address["name"] = Value::String(name);
            }
            json!({ "emailAddress": address })
        })
        .collect())
}

/// Plain text as an HTML block for a reply body.
fn text_to_html(text: &str) -> String {
    let escaped = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\n', "<br>");
    format!(
        "<div style=\"font-family:Calibri,Arial,sans-serif;font-size:11pt\">{escaped}</div><br>"
    )
}

/// Graph ids can contain `/`, `+` and `=`; immutable ids are URL-safe base64.
fn path(id: &str) -> String {
    url::form_urlencoded::byte_serialize(id.as_bytes()).collect()
}

/// `2026-10-21T14:00:00Z` (Graph's format) from Unix ms.
fn iso_date(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let t = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        t / 3600,
        t % 3600 / 60,
        t % 60
    )
}

/// Unix ms from `2026-10-21T14:00:00Z` or with fractional seconds.
fn parse_iso_date(s: &str) -> Option<i64> {
    let s = s.trim().trim_end_matches('Z');
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>());
    let (y, m, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let mut t = time.split(':');
    let (h, min) = (
        t.next()?.parse::<i64>().ok()?,
        t.next()?.parse::<i64>().ok()?,
    );
    let sec: f64 = t.next().unwrap_or("0").parse().ok()?;
    let days = days_from_civil(y, m, day);
    Some((days * 86_400 + h * 3600 + min * 60) * 1000 + (sec * 1000.0).round() as i64)
}

/// Howard Hinnant's date algorithms.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn lock(store: &Mutex<Store>) -> std::sync::MutexGuard<'_, Store> {
    store
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_round_trip() {
        let ms = 1_792_591_200_000; // 2026-10-21 14:00 UTC
        assert_eq!(iso_date(ms), "2026-10-21T14:00:00Z");
        assert_eq!(parse_iso_date("2026-10-21T14:00:00Z"), Some(ms));
        assert_eq!(parse_iso_date("2026-10-21T14:00:00.250Z"), Some(ms + 250));
        assert_eq!(
            parse_iso_date("2024-02-29T00:00:00Z"),
            Some(1_709_164_800_000)
        );
        assert_eq!(parse_iso_date("nonsense"), None);
        let url = first_delta_url(ms);
        assert!(
            url.ends_with("$filter=receivedDateTime+ge+2026-09-21T14:00:00Z"),
            "{url}"
        );
    }

    #[test]
    fn graph_messages_become_incoming_mail() {
        let m: GraphMessage = serde_json::from_value(json!({
            "id": "AAMkAD/1+x=",
            "subject": " Q4 plan ",
            "from": { "emailAddress": { "name": "Ana Ruiz", "address": "ana@contoso.com" } },
            "toRecipients": [
                { "emailAddress": { "name": "Me", "address": "me@contoso.com" } },
                { "emailAddress": { "name": "bob@contoso.com", "address": "bob@contoso.com" } }
            ],
            "receivedDateTime": "2026-10-21T14:00:00Z",
            "isRead": false,
            "flag": { "flagStatus": "flagged" },
            "bodyPreview": "Hi team,\r\nthe plan is attached.",
            "body": { "contentType": "html", "content": "<p>Hi team,</p><p>the plan is attached.</p>" },
            "hasAttachments": true,
            "internetMessageId": "<abc@contoso.com>",
            "conversationId": "conv1",
            "inferenceClassification": "focused"
        }))
        .unwrap();
        let i = to_incoming(3, &m);
        assert_eq!(i.remote_id.as_deref(), Some("AAMkAD/1+x="));
        assert_eq!(i.message_id.as_deref(), Some("abc@contoso.com"));
        assert_eq!(i.from_name, "Ana Ruiz");
        assert_eq!(i.to, vec!["Me <me@contoso.com>", "bob@contoso.com"]);
        assert_eq!(i.subject, "Q4 plan");
        assert_eq!(i.snippet, "Hi team, the plan is attached.");
        assert!(i.text.contains("the plan is attached"));
        assert!(i.html.is_some());
        assert_eq!(i.date, 1_792_591_200_000);
        assert!(i.unread && i.flagged);
        assert_eq!(i.category, Category::People);
        assert_eq!(path(&m.id), "AAMkAD%2F1%2Bx%3D");

        let other: GraphMessage = serde_json::from_value(json!({
            "id": "x", "isRead": true, "inferenceClassification": "other",
            "from": { "emailAddress": { "address": "deals@shop.com" } }
        }))
        .unwrap();
        let i = to_incoming(3, &other);
        assert_eq!(i.category, Category::Newsletters);
        assert_eq!(i.from_name, "deals@shop.com");
    }

    #[test]
    fn removed_entries_parse() {
        let page: DeltaPage = serde_json::from_value(json!({
            "value": [ { "id": "gone1", "@removed": { "reason": "deleted" } } ],
            "@odata.deltaLink": "https://graph.microsoft.com/v1.0/me/mailFolders/inbox/messages/delta?$deltatoken=abc"
        }))
        .unwrap();
        assert!(page.value[0].removed.is_some());
        assert!(page.delta_link.is_some() && page.next_link.is_none());
    }

    fn pending(place: Place, remote_place: Place, remote_folder: Option<i64>) -> Pending {
        Pending {
            id: 1,
            remote_id: Some("AAMk1".into()),
            mailbox: None,
            uid: None,
            message_id: None,
            place,
            folder: None,
            unread: false,
            pinned: false,
            remote_place,
            remote_folder,
            remote_unread: false,
            remote_pinned: false,
        }
    }

    #[test]
    fn filed_mail_moves_to_its_home_folder_by_id() {
        let clients = Some((5, "AAMk-clients".to_owned()));
        // Into a folder from the inbox, or from another folder.
        let from_inbox = Pending {
            folder: clients.clone(),
            ..pending(Place::Folder, Place::Inbox, None)
        };
        assert_eq!(move_destination(&from_inbox), Some("AAMk-clients"));
        let from_projects = Pending {
            folder: clients.clone(),
            ..pending(Place::Folder, Place::Folder, Some(4))
        };
        assert_eq!(move_destination(&from_projects), Some("AAMk-clients"));
        // Already there (e.g. only read): no move.
        let read = Pending {
            folder: clients.clone(),
            ..pending(Place::Folder, Place::Folder, Some(5))
        };
        assert_eq!(move_destination(&read), None);
        // Done undone after it reached Archive: back into the folder.
        let undone = Pending {
            folder: clients,
            ..pending(Place::Folder, Place::Archive, None)
        };
        assert_eq!(move_destination(&undone), Some("AAMk-clients"));
        // Done, back to the inbox, and filed without a known folder.
        assert_eq!(
            move_destination(&pending(Place::Archive, Place::Folder, Some(5))),
            Some("archive")
        );
        assert_eq!(
            move_destination(&pending(Place::Inbox, Place::Folder, Some(5))),
            Some("inbox")
        );
        assert_eq!(
            move_destination(&pending(Place::Folder, Place::Folder, Some(5))),
            None
        );
    }

    #[test]
    fn drafts_are_sent_as_text_with_every_recipient_field() {
        let draft = Draft {
            account: 1,
            to: "Ana Ruiz <ana@contoso.com>; bob@".into(),
            cc: String::new(),
            bcc: "boss@contoso.com".into(),
            subject: " Plan ".into(),
            body: "Draft text".into(),
            reply_to: None,
            send_at: 0,
            attachments: Vec::new(),
            draft_id: Some(4),
            in_reply_to: None,
            references: None,
        };
        assert_eq!(
            draft_message(&draft),
            json!({
                "subject": "Plan",
                "body": { "contentType": "Text", "content": "Draft text" },
                "toRecipients": [
                    { "emailAddress": { "address": "ana@contoso.com", "name": "Ana Ruiz" } }
                ],
                "ccRecipients": [],
                "bccRecipients": [ { "emailAddress": { "address": "boss@contoso.com" } } ],
            })
        );
        let m: GraphMessage = serde_json::from_value(json!({
            "id": "d1",
            "bccRecipients": [ { "emailAddress": { "name": "Boss", "address": "boss@contoso.com" } } ]
        }))
        .unwrap();
        assert_eq!(to_incoming(3, &m).bcc, ["Boss <boss@contoso.com>"]);
    }

    #[test]
    fn reply_text_is_escaped() {
        assert_eq!(
            text_to_html("a < b\nc & d"),
            "<div style=\"font-family:Calibri,Arial,sans-serif;font-size:11pt\">a &lt; b<br>c &amp; d</div><br>"
        );
    }
}
