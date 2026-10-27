//! Microsoft Graph for Microsoft 365 mailboxes: the inbox through delta
//! queries over its recent mail (older mail comes in as the user scrolls to
//! it), other folders' newest messages, flag and move changes, folder
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
    Added, Attachment, Category, Folder, FolderRole, Incoming, Pending, Place, Pushed, Query,
    RemoteFolder, Store,
};

use crate::attachment::{
    AttachmentData, INLINE_IMAGE_MAX, INLINE_TOTAL_MAX, InlineImage, cid_key, mime_for,
};
use crate::draft::{Bodies, Draft, recipients};
use crate::invite::Invite;
use crate::parse::{automated_sender, bare_id, html_to_text, snippet};
use crate::search::{self, SERVER_HITS};
use crate::{BLOCKED, Error, OLDER_STEP, Result};

pub const GRAPH: &str = "https://graph.microsoft.com/v1.0";
/// How far back the first sync of a Microsoft account reaches.
pub const FIRST_SYNC_DAYS: i64 = 30;
const DAY_MS: i64 = 86_400_000;
const HOST: &str = "graph.microsoft.com";
/// Graph takes attachments smaller than this in one request; bigger ones go
/// up through an upload session.
const INLINE_ATTACHMENT_MAX: usize = 3 * 1024 * 1024;
/// What Outlook writes in by default; typed text goes out in it.
const OUTLOOK_BODY: &str = "font-family:Calibri,Arial,sans-serif;font-size:11pt";
/// Upload session chunks must be multiples of 320 KiB.
const UPLOAD_CHUNK: usize = 10 * 320 * 1024;
/// A folder shows its newest this many messages, until the user reaches
/// further back.
pub const FOLDER_LIMIT: usize = 200;
/// Graph's biggest page of messages.
const MAX_PAGE: usize = 1000;
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

const SELECT: &str = "subject,from,replyTo,toRecipients,ccRecipients,bccRecipients,receivedDateTime,\
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

/// Where an account's sync stands, kept as JSON in the store.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncState {
    pub delta_link: Option<String>,
    /// The delta query selects Reply-To. Older cursors need one full header
    /// refresh because Graph keeps the original query's selected fields.
    #[serde(default)]
    pub reply_to_selected: bool,
    /// Where the delta query's window starts (Unix ms); older mail comes
    /// through `older_inbox`.
    #[serde(default)]
    pub since: Option<i64>,
    /// The oldest inbox mail reached back for so far (Unix ms).
    #[serde(default)]
    pub older_than: Option<i64>,
    /// Nothing older is left in the inbox.
    #[serde(default)]
    pub complete: bool,
}

/// Where a folder's sync stands, kept as JSON with the folder.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FolderState {
    /// How many of its newest messages the folder shows; `FOLDER_LIMIT`
    /// until the user reaches further back.
    pub limit: Option<usize>,
    /// The folder has no messages beyond the limit.
    pub complete: bool,
}

/// A message as a listing gives it: enough to tell whether it's stored,
/// and its flags.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Listed {
    id: String,
    received_date_time: Option<String>,
    is_read: Option<bool>,
    flag: Option<GraphFlag>,
    /// Only searches ask for it.
    #[serde(default)]
    parent_folder_id: Option<String>,
    reply_to: Option<Vec<GraphRecipient>>,
}
const LISTED: &str = "id,receivedDateTime,isRead,flag,replyTo";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Ids of the messages stored for the first time.
    pub new: Vec<i64>,
    pub gone: usize,
    /// Cached messages whose Reply-To headers changed.
    pub headers: usize,
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
    /// `#microsoft.graph.eventMessageRequest` and the like for meeting
    /// messages; absent for plain mail.
    #[serde(rename = "@odata.type")]
    pub odata_type: Option<String>,
    pub subject: Option<String>,
    pub from: Option<GraphRecipient>,
    pub reply_to: Option<Vec<GraphRecipient>>,
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
pub(crate) struct Page<T> {
    #[serde(default = "Vec::new")]
    pub(crate) value: Vec<T>,
    #[serde(rename = "@odata.nextLink")]
    pub(crate) next_link: Option<String>,
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

    pub(crate) fn request(&self, method: Method, url: &str) -> RequestBuilder {
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

    pub(crate) async fn send(&self, request: RequestBuilder) -> Result<reqwest::Response> {
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

    pub(crate) async fn json<T: for<'de> Deserialize<'de>>(
        &self,
        request: RequestBuilder,
    ) -> Result<T> {
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
        let replacing_legacy_cursor = !state.reply_to_selected && state.delta_link.is_some();
        if !state.reply_to_selected {
            state.delta_link = None;
        }
        let first = state.delta_link.is_none();
        let mut report = SyncReport {
            first,
            ..Default::default()
        };
        let mut url = match &state.delta_link {
            Some(link) => link.clone(),
            None => {
                let since = state
                    .since
                    .unwrap_or_else(|| first_sync_since(spark_store::now_ms()));
                state.since = Some(since);
                first_delta_url(since).replacen(GRAPH, &self.base, 1)
            }
        };
        // A fresh snapshot omits messages removed before it began. Reconcile
        // those cached rows when replacing an old cursor, within its window.
        let mut missing = HashSet::new();
        if replacing_legacy_cursor {
            let store = lock(store);
            let since = state.since.expect("a fresh query sets its window");
            for message in store.messages()? {
                if message.account_id == account_id
                    && message.date >= since
                    && let Some(location) = store.location(message.id)?
                    && location.remote_place == Place::Inbox
                    && let Some(remote_id) = location.remote_id
                {
                    missing.insert(remote_id);
                }
            }
        }
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
                missing.remove(&m.id);
                if m.removed.is_some() {
                    report.gone += lock(store).observe_gone_remote(account_id, &m.id)?;
                    continue;
                }
                let mut incoming = to_incoming(account_id, &m);
                report.headers += usize::from(lock(store).observe_reply_to(&incoming)?);
                // A header-only delta must not overwrite the stored flags.
                if m.is_read.is_none() {
                    continue;
                }
                if !lock(store).has_remote_id(account_id, &m.id)? {
                    self.complete(&m, &mut incoming).await;
                }
                if let spark_store::Added::New(id) = lock(store).add_incoming(&incoming, !first)? {
                    report.new.push(id);
                }
            }
            match (page.next_link, page.delta_link) {
                (Some(next), _) => url = next,
                (None, Some(delta)) => {
                    for remote_id in &missing {
                        report.gone += lock(store).observe_gone_remote(account_id, remote_id)?;
                    }
                    state.delta_link = Some(delta);
                    state.reply_to_selected = true;
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
                    let role = well_known.get(&f.id).copied().flatten().or_else(|| {
                        (parent.is_none() && f.display_name.eq_ignore_ascii_case(BLOCKED))
                            .then_some(FolderRole::Blocked)
                    });
                    folders.push(RemoteFolder {
                        role,
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

    /// Brings a folder's newest messages (up to its limit) into the store
    /// and makes them its list. Returns how many came or went, or `None` if
    /// the folder is gone.
    pub async fn sync_folder(
        &self,
        store: &Mutex<Store>,
        folder: &Folder,
        state: &mut FolderState,
    ) -> Result<Option<usize>> {
        let account_id = folder.account_id;
        let limit = state.limit.unwrap_or(FOLDER_LIMIT);
        let messages = format!(
            "/me/mailFolders/{}/messages?$orderby=receivedDateTime+desc",
            path(&folder.remote_id)
        );
        let mut listing: Vec<Listed> = Vec::new();
        let mut next = Some(format!(
            "{messages}&$top={}&$select={LISTED}",
            limit.min(MAX_PAGE)
        ));
        let mut ended = false;
        while let Some(url) = next.take() {
            let page: Page<Listed> = match self.json(self.request(Method::GET, &url)).await {
                Err(Error::Gone) => return Ok(None),
                other => other?,
            };
            listing.extend(page.value);
            match page.next_link {
                Some(link) if listing.len() < limit => next = Some(link),
                Some(_) => {}
                None => ended = true,
            }
        }
        state.complete = ended && listing.len() <= limit;
        listing.truncate(limit);
        let (before, known): (HashSet<i64>, HashSet<String>) = {
            let store = lock(store);
            let mut known = HashSet::new();
            for m in &listing {
                if store.has_remote_id(account_id, &m.id)? {
                    known.insert(m.id.clone());
                }
            }
            (store.folder_member_ids(folder.id)?, known)
        };
        let full = self.unknown_in_full(&messages, 0, &listing, &known).await?;

        let mut changed = 0;
        let mut members = Vec::new();
        for listed in &listing {
            let unread = !listed.is_read.unwrap_or(false);
            let flagged = listed
                .flag
                .as_ref()
                .is_some_and(|f| f.flag_status == "flagged");
            let incoming = if known.contains(&listed.id) {
                Incoming {
                    account_id,
                    remote_id: Some(listed.id.clone()),
                    reply_to: listed
                        .reply_to
                        .as_ref()
                        .map(|recipients| recipients.iter().filter_map(display).collect()),
                    unread,
                    flagged,
                    ..Default::default()
                }
            } else if let Some(m) = full.get(&listed.id) {
                let mut incoming = to_incoming(account_id, m);
                self.complete(m, &mut incoming).await;
                incoming
            } else {
                continue;
            };
            changed += usize::from(lock(store).observe_reply_to(&incoming)?);
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

    /// The messages of a listing that aren't in `known`, in full: one by
    /// one when there are few, else by listing again with bodies over the
    /// stretch that holds them. `messages` is the listing's path with its
    /// filter and order; `listed` what it listed from position `offset` on.
    /// Messages moved or deleted since are left out.
    async fn unknown_in_full(
        &self,
        messages: &str,
        offset: usize,
        listed: &[Listed],
        known: &HashSet<String>,
    ) -> Result<HashMap<String, GraphMessage>> {
        let unknown = |m: &Listed| !known.contains(&m.id);
        let mut full: HashMap<String, GraphMessage> = HashMap::new();
        let (Some(first), Some(last)) = (
            listed.iter().position(unknown),
            listed.iter().rposition(unknown),
        ) else {
            return Ok(full);
        };
        if listed.iter().filter(|m| unknown(m)).count() > FETCH_ONE_BY_ONE {
            let mut covered = offset + first;
            let mut next = Some(format!(
                "{messages}&$top=50&$skip={covered}&$select={SELECT}"
            ));
            while let Some(url) = next.take() {
                let page: Page<GraphMessage> = self
                    .json(
                        self.request(Method::GET, &url)
                            .header("Prefer", "outlook.body-content-type=\"html\""),
                    )
                    .await?;
                covered += page.value.len();
                full.extend(page.value.into_iter().map(|m| (m.id.clone(), m)));
                if covered <= offset + last {
                    next = page.next_link;
                }
            }
        } else {
            for m in listed.iter().filter(|m| unknown(m)) {
                if let Some(m) = self.message_in_full(&m.id).await? {
                    full.insert(m.id.clone(), m);
                }
            }
        }
        Ok(full)
    }

    /// A message with its body; `None` if it was moved or deleted since it
    /// was listed.
    async fn message_in_full(&self, id: &str) -> Result<Option<GraphMessage>> {
        match self
            .json(
                self.request(
                    Method::GET,
                    &format!("/me/messages/{}?$select={SELECT}", path(id)),
                )
                .header("Prefer", "outlook.body-content-type=\"html\""),
            )
            .await
        {
            Ok(m) => Ok(Some(m)),
            Err(Error::Gone) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Reaches further back in the inbox than the delta query: stores the
    /// next `OLDER_STEP` messages received before the oldest reached so far
    /// (at first, before the delta window), as on a first sync: no
    /// screening. Returns how many were new.
    ///
    /// The delta query only covers its own window, so changes made
    /// elsewhere to mail this old (read, flagged, moved, deleted) don't
    /// reach the store.
    pub async fn older_inbox(
        &self,
        store: &Mutex<Store>,
        account_id: i64,
        state: &mut SyncState,
    ) -> Result<usize> {
        if state.delta_link.is_none() {
            return Ok(0);
        }
        let before = match state.older_than {
            // The rest of that second too: some of it may not be listed yet.
            Some(oldest) => oldest + 1000,
            // Accounts synced before `since` was kept had the default window.
            None => state
                .since
                .unwrap_or_else(|| first_sync_since(spark_store::now_ms())),
        };
        let messages = format!(
            "/me/mailFolders/inbox/messages?$filter=receivedDateTime+lt+{}&$orderby=receivedDateTime+desc",
            iso_date(before)
        );
        let mut next = Some(format!("{messages}&$top={OLDER_STEP}&$select={LISTED}"));
        let mut offset = 0;
        let mut stored = 0;
        while let Some(url) = next.take() {
            let page: Page<Listed> = self.json(self.request(Method::GET, &url)).await?;
            let known: HashSet<String> = {
                let store = lock(store);
                let mut known = HashSet::new();
                for m in &page.value {
                    if store.has_remote_id(account_id, &m.id)? {
                        known.insert(m.id.clone());
                    }
                }
                known
            };
            let full = self
                .unknown_in_full(&messages, offset, &page.value, &known)
                .await?;
            for listed in &page.value {
                let Some(m) = full.get(&listed.id) else {
                    continue;
                };
                let mut incoming = to_incoming(account_id, m);
                self.complete(m, &mut incoming).await;
                if let Added::New(_) = lock(store).add_incoming(&incoming, false)? {
                    stored += 1;
                }
            }
            offset += page.value.len();
            if let Some(oldest) = page
                .value
                .iter()
                .filter_map(|m| m.received_date_time.as_deref().and_then(parse_iso_date))
                .min()
            {
                state.older_than = Some(state.older_than.map_or(oldest, |o| o.min(oldest)));
            }
            match page.next_link {
                // Mail already stored doesn't count: an account whose
                // window lagged behind lists past it.
                Some(link) if stored < OLDER_STEP => next = Some(link),
                Some(_) => {}
                None => state.complete = true,
            }
        }
        Ok(stored)
    }

    /// Puts a search to the mailbox (all its folders) and stores the newest
    /// `SERVER_HITS` hits that aren't stored yet: inbox mail older than the
    /// delta window as reaching back stores it (newer mail is the delta
    /// query's), and mail in stored folders as their syncs do. Mail in
    /// Deleted Items, Junk Email or Drafts, or in folders not listed, is
    /// left out, as Spark does. Returns how many were new. Does nothing
    /// before the first sync.
    pub async fn search_server(
        &self,
        store: &Mutex<Store>,
        account_id: i64,
        state: &SyncState,
        query: &Query,
    ) -> Result<usize> {
        let Some(kql) = search::kql(query) else {
            return Ok(0);
        };
        if state.delta_link.is_none() {
            return Ok(0);
        }
        let since = state
            .since
            .unwrap_or_else(|| first_sync_since(spark_store::now_ms()));
        let inbox: GraphFolder = self
            .json(self.request(Method::GET, "/me/mailFolders/inbox?$select=id"))
            .await?;
        let folders: HashMap<String, Folder> = lock(store)
            .folders()?
            .into_iter()
            .filter(|f| {
                f.account_id == account_id
                    && !matches!(
                        f.role,
                        Some(
                            FolderRole::Trash
                                | FolderRole::Spam
                                | FolderRole::Drafts
                                | FolderRole::Blocked
                        )
                    )
            })
            .map(|f| (f.remote_id.clone(), f))
            .collect();
        // Newest first, which is how Graph sorts what it finds.
        let found = format!(
            "/me/messages?$search={}&$top={SERVER_HITS}",
            path(&format!("\"{kql}\""))
        );
        let listing: Page<Listed> = self
            .json(self.request(
                Method::GET,
                &format!("{found}&$select={LISTED},parentFolderId"),
            ))
            .await?;
        // Where each new one goes: the inbox (`None`) or a stored folder.
        let mut wanted: Vec<(&Listed, Option<&Folder>)> = Vec::new();
        {
            let store = lock(store);
            for m in &listing.value {
                if store.has_remote_id(account_id, &m.id)? {
                    continue;
                }
                let parent = m.parent_folder_id.as_deref().unwrap_or_default();
                if parent == inbox.id {
                    let received = m.received_date_time.as_deref().and_then(parse_iso_date);
                    if received.is_some_and(|r| r < since) {
                        wanted.push((m, None));
                    }
                } else if let Some(folder) = folders.get(parent) {
                    wanted.push((m, Some(folder)));
                }
            }
        }
        let mut full: HashMap<String, GraphMessage> = HashMap::new();
        if wanted.len() > FETCH_ONE_BY_ONE {
            let page: Page<GraphMessage> = self
                .json(
                    self.request(Method::GET, &format!("{found}&$select={SELECT}"))
                        .header("Prefer", "outlook.body-content-type=\"html\""),
                )
                .await?;
            full.extend(page.value.into_iter().map(|m| (m.id.clone(), m)));
        } else {
            for (m, _) in &wanted {
                if let Some(m) = self.message_in_full(&m.id).await? {
                    full.insert(m.id.clone(), m);
                }
            }
        }
        let mut stored = 0;
        for (listed, folder) in wanted {
            let Some(m) = full.get(&listed.id) else {
                continue;
            };
            let mut incoming = to_incoming(account_id, m);
            self.complete(m, &mut incoming).await;
            let added = match folder {
                None => lock(store).add_incoming(&incoming, false)?,
                Some(folder) => lock(store).add_folder_message(folder.id, &incoming)?,
            };
            if let Added::New(_) = added {
                stored += 1;
            }
        }
        Ok(stored)
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

    /// Of `senders`, the ones the user has written to: Sent Items has mail
    /// to them. Search matches more than the exact address, so the
    /// candidates' recipients are checked.
    pub async fn written_to(&self, senders: &[String]) -> Result<Vec<String>> {
        let mut found = Vec::new();
        for address in senders.iter().filter(|a| !a.contains(['"', '\\'])) {
            let search = format!("\"recipients:{address}\"");
            let page: Page<Value> = self
                .json(self.request(
                    Method::GET,
                    &format!(
                        "/me/mailFolders/sentitems/messages?$search={}&$top=10\
                         &$select=toRecipients,ccRecipients,bccRecipients",
                        path(&search)
                    ),
                ))
                .await?;
            let to = |m: &Value| {
                ["toRecipients", "ccRecipients", "bccRecipients"]
                    .iter()
                    .filter_map(|field| m[field].as_array())
                    .flatten()
                    .filter_map(|r| r["emailAddress"]["address"].as_str())
                    .any(|a| a.eq_ignore_ascii_case(address))
            };
            if page.value.iter().any(to) {
                found.push(address.clone());
            }
        }
        Ok(found)
    }

    /// The id of the mailbox's Blocked folder (at the top, as Spark makes
    /// it), made if there isn't one.
    async fn blocked_folder(&self) -> Result<String> {
        let filter = format!("displayName eq '{BLOCKED}'");
        let found: Page<GraphFolder> = self
            .json(self.request(
                Method::GET,
                &format!(
                    "/me/mailFolders?$filter={}&$select={FOLDER_SELECT}",
                    path(&filter)
                ),
            ))
            .await?;
        match found.value.into_iter().next() {
            Some(f) => Ok(f.id),
            None => self.create_folder(None, BLOCKED).await,
        }
    }

    /// What a listing leaves out of a new message: its attachments, and
    /// the invite a meeting message carries.
    async fn complete(&self, m: &GraphMessage, incoming: &mut Incoming) {
        if m.has_attachments == Some(true) {
            incoming.attachments = self.attachments(&m.id).await.unwrap_or_default();
        }
        if m.odata_type
            .as_deref()
            .is_some_and(|t| t.contains("eventMessage"))
        {
            incoming.invite = self.invite(&m.id).await.map(|i| i.to_json());
        }
    }

    /// The invite in a meeting message. Exchange keeps meeting messages as
    /// items of their own and writes the calendar part out only in their MIME.
    async fn invite(&self, id: &str) -> Option<Invite> {
        let response = self
            .send(self.request(Method::GET, &format!("/me/messages/{}/$value", path(id))))
            .await
            .ok()?;
        Invite::from_raw(&response.bytes().await.ok()?)
    }

    /// Sends a message given whole as MIME: one with parts Graph's JSON
    /// can't express, like a calendar reply. It's saved in Sent Items.
    pub async fn send_mime(&self, message: &[u8]) -> Result<()> {
        self.send(
            self.request(Method::POST, "/me/sendMail")
                .header("Content-Type", "text/plain")
                .body(STANDARD.encode(message)),
        )
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

    /// The images of message `id` whose Content-IDs (as `cid_key` gives
    /// them) are in `wanted`. Inline attachments are tried first; other
    /// images only if some are still missing, as a few mailers send them so.
    pub async fn inline_images(&self, id: &str, wanted: &[String]) -> Result<Vec<InlineImage>> {
        #[derive(Deserialize)]
        struct Page {
            value: Vec<Listed>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Listed {
            id: String,
            content_type: Option<String>,
            size: Option<usize>,
            is_inline: Option<bool>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Full {
            name: Option<String>,
            content_type: Option<String>,
            content_id: Option<String>,
            content_bytes: Option<String>,
        }
        let page: Page = self
            .json(self.request(
                Method::GET,
                &format!(
                    "/me/messages/{}/attachments?$select=id,contentType,size,isInline",
                    path(id)
                ),
            ))
            .await?;
        let mut candidates: Vec<Listed> = page
            .value
            .into_iter()
            .filter(|a| {
                a.is_inline == Some(true)
                    || a.content_type
                        .as_deref()
                        .is_some_and(|t| t.to_ascii_lowercase().starts_with("image/"))
            })
            // Graph's size counts the item's metadata too; this only skips giants.
            .filter(|a| a.size.unwrap_or(0) <= INLINE_IMAGE_MAX * 2)
            .collect();
        candidates.sort_by_key(|a| a.is_inline != Some(true));

        let mut images: Vec<InlineImage> = Vec::new();
        let mut total = 0;
        for listed in candidates {
            if images.len() == wanted.len() {
                break;
            }
            let full: Full = match self
                .json(self.request(
                    Method::GET,
                    &format!("/me/messages/{}/attachments/{}", path(id), path(&listed.id)),
                ))
                .await
            {
                Ok(full) => full,
                // Removed since the listing.
                Err(Error::Gone) => continue,
                Err(e) => return Err(e),
            };
            let Some(cid) = full.content_id.as_deref().map(cid_key) else {
                continue;
            };
            if !wanted.contains(&cid) || images.iter().any(|i| i.cid == cid) {
                continue;
            }
            let Some(mime) = InlineImage::image_type(
                full.content_type.as_deref().unwrap_or_default(),
                full.name.as_deref(),
            ) else {
                continue;
            };
            let Some(bytes) = full.content_bytes.and_then(|b| STANDARD.decode(b).ok()) else {
                continue;
            };
            if bytes.is_empty()
                || bytes.len() > INLINE_IMAGE_MAX
                || total + bytes.len() > INLINE_TOTAL_MAX
            {
                continue;
            }
            total += bytes.len();
            images.push(InlineImage { cid, mime, bytes });
        }
        Ok(images)
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
        let blocked = if pending.iter().any(|p| p.place == Place::Blocked) {
            Some(self.blocked_folder().await?)
        } else {
            None
        };
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
                if let Some(destination) = move_destination(&p, blocked.as_deref()) {
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
    ) -> Result<Option<String>> {
        let to = graph_recipients(&draft.to)?;
        let cc = graph_recipients(&draft.cc)?;
        let bcc = graph_recipients(&draft.bcc)?;
        if to.is_empty() && cc.is_empty() && bcc.is_empty() {
            return Err(Error::Config("Add at least one recipient.".into()));
        }
        let bodies = draft.bodies(None, Some(OUTLOOK_BODY));
        let Some(original) = reply_to else {
            let message = json!({
                "subject": draft.subject.trim(),
                "body": body(&bodies),
                "toRecipients": to, "ccRecipients": cc, "bccRecipients": bcc,
            });
            // Sent from a draft, attachments or not, so its Message-ID is
            // known (for reminders); Graph keeps it when it's sent.
            let created: GraphMessage = self
                .json(self.request(Method::POST, "/me/messages").json(&message))
                .await?;
            self.attach_and_send(
                &format!("/me/messages/{}", path(&created.id)),
                files,
                &bodies.images,
            )
            .await?;
            return Ok(created.internet_message_id.as_deref().map(bare_id));
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
        let ours = bodies
            .html
            .clone()
            .unwrap_or_else(|| text_to_html(&draft.body));
        let update = json!({
            "subject": draft.subject.trim(),
            "body": { "contentType": "HTML", "content": format!("{ours}{quoted}") },
            "toRecipients": to, "ccRecipients": cc, "bccRecipients": bcc,
        });
        let draft_path = format!("/me/messages/{}", path(&reply.id));
        self.send(self.request(Method::PATCH, &draft_path).json(&update))
            .await?;
        self.attach_and_send(&draft_path, files, &bodies.images)
            .await?;
        Ok(reply.internet_message_id.as_deref().map(bare_id))
    }

    /// Adds `files` and the signature's `images` to a draft and sends it.
    /// If that fails, the draft is deleted so it doesn't linger in Drafts.
    async fn attach_and_send(
        &self,
        draft_path: &str,
        files: &[AttachmentData],
        images: &[InlineImage],
    ) -> Result<()> {
        let result = async {
            for image in images {
                self.attach_inline(draft_path, image).await?;
            }
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

    /// An image the HTML shows by its `cid:`. Signature images are small
    /// enough to go in one request.
    async fn attach_inline(&self, draft_path: &str, image: &InlineImage) -> Result<()> {
        let extension = image.mime.rsplit('/').next().unwrap_or("png");
        self.send(
            self.request(Method::POST, &format!("{draft_path}/attachments"))
                .json(&json!({
                    "@odata.type": "#microsoft.graph.fileAttachment",
                    "name": format!("image.{extension}"),
                    "contentType": image.mime,
                    "contentBytes": STANDARD.encode(&image.bytes),
                    "isInline": true,
                    "contentId": image.cid,
                })),
        )
        .await?;
        Ok(())
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
/// id of the folder it's filed into or of the Blocked folder (`blocked`).
/// `None` if it stays where it is.
fn move_destination<'a>(p: &'a Pending, blocked: Option<&'a str>) -> Option<&'a str> {
    let destination = match p.place {
        Place::Inbox => "inbox",
        Place::Archive => "archive",
        Place::Trash => "deleteditems",
        Place::Blocked => blocked?,
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
    /// A draft that's gone is made anew, and so is one whose signature has
    /// images: they're attached with the draft.
    pub async fn save_draft(
        &self,
        update: Option<&str>,
        draft: &Draft,
        files: &[AttachmentData],
    ) -> Result<(String, Option<String>)> {
        let bodies = draft.bodies(None, Some(OUTLOOK_BODY));
        let message = draft_message(draft, &bodies);
        let update = update.filter(|_| bodies.images.is_empty());
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
        let attached = async {
            for image in &bodies.images {
                self.attach_inline(&draft_path, image).await?;
            }
            for f in files {
                self.attach(&draft_path, f).await?;
            }
            Ok::<_, Error>(())
        }
        .await;
        if let Err(e) = attached {
            let _ = self.send(self.request(Method::DELETE, &draft_path)).await;
            return Err(e);
        }
        Ok((
            created.id,
            created.internet_message_id.as_deref().map(bare_id),
        ))
    }

    /// A message's internet headers, as name and value.
    pub async fn headers(&self, id: &str) -> Result<Vec<(String, String)>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Headers {
            #[serde(default)]
            internet_message_headers: Vec<Header>,
        }
        #[derive(Deserialize)]
        struct Header {
            name: String,
            value: String,
        }
        let found: Headers = self
            .json(self.request(
                Method::GET,
                &format!("/me/messages/{}?$select=internetMessageHeaders", path(id)),
            ))
            .await?;
        Ok(found
            .internet_message_headers
            .into_iter()
            .map(|h| (h.name, h.value))
            .collect())
    }

    /// Deletes a draft for good, as Outlook does once it's sent; where
    /// that isn't offered, into Deleted Items.
    pub async fn delete_draft(&self, id: &str) -> Result<()> {
        self.delete_forever(id).await
    }

    /// Deletes a message for good; where that isn't offered, into Deleted
    /// Items (or out of it, into Recoverable Items). One that's gone already
    /// is fine.
    pub async fn delete_forever(&self, id: &str) -> Result<()> {
        let message_path = format!("/me/messages/{}", path(id));
        let deleted = self
            .send(
                self.request(Method::POST, &format!("{message_path}/permanentDelete"))
                    .json(&json!({})),
            )
            .await;
        let deleted = match deleted {
            Err(Error::Server(_)) => self.send(self.request(Method::DELETE, &message_path)).await,
            other => other,
        };
        match deleted {
            Ok(_) | Err(Error::Gone) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Deletes every message in a folder for good, a page at a time.
    /// Returns how many there were.
    pub async fn empty_folder(&self, folder_id: &str) -> Result<usize> {
        let page = format!(
            "/me/mailFolders/{}/messages?$select=id&$top={EMPTY_PAGE}",
            path(folder_id)
        );
        let mut deleted = 0;
        loop {
            let listed: Page<Listed> = self.json(self.request(Method::GET, &page)).await?;
            if listed.value.is_empty() {
                return Ok(deleted);
            }
            for m in &listed.value {
                self.delete_forever(&m.id).await?;
            }
            deleted += listed.value.len();
        }
    }
}

/// Messages deleted per page when emptying a folder.
const EMPTY_PAGE: usize = 100;

/// A message body as Graph takes it: HTML with a signature, else text.
fn body(bodies: &Bodies) -> Value {
    match &bodies.html {
        Some(html) => json!({ "contentType": "HTML", "content": html }),
        None => json!({ "contentType": "Text", "content": bodies.text }),
    }
}

/// A draft as Graph takes it, with its `bodies`. Addresses that don't parse
/// yet are left out.
pub fn draft_message(draft: &Draft, bodies: &Bodies) -> Value {
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
        "body": body(bodies),
        "toRecipients": list(&draft.to),
        "ccRecipients": list(&draft.cc),
        "bccRecipients": list(&draft.bcc),
    })
}

/// Where the first sync's window starts: `FIRST_SYNC_DAYS` before now.
pub fn first_sync_since(now_ms: i64) -> i64 {
    now_ms - FIRST_SYNC_DAYS * DAY_MS
}

/// The first delta request: the inbox's mail received since `since_ms`.
pub fn first_delta_url(since_ms: i64) -> String {
    let since = iso_date(since_ms);
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
        reply_to: m
            .reply_to
            .as_ref()
            .map(|recipients| recipients.iter().filter_map(display).collect()),
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
    format!("<div style=\"{OUTLOOK_BODY}\">{escaped}</div><br>")
}

/// Graph ids can contain `/`, `+` and `=`; immutable ids are URL-safe base64.
pub(crate) fn path(id: &str) -> String {
    url::form_urlencoded::byte_serialize(id.as_bytes()).collect()
}

/// `2026-10-21T14:00:00Z` (Graph's format) from Unix ms.
pub(crate) fn iso_date(ms: i64) -> String {
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
pub(crate) fn parse_iso_date(s: &str) -> Option<i64> {
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
pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub(crate) fn civil_from_days(days: i64) -> (i64, i64, i64) {
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
pub(crate) mod tests {
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
        let url = first_delta_url(first_sync_since(ms));
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
        assert_eq!(move_destination(&from_inbox, None), Some("AAMk-clients"));
        let from_projects = Pending {
            folder: clients.clone(),
            ..pending(Place::Folder, Place::Folder, Some(4))
        };
        assert_eq!(move_destination(&from_projects, None), Some("AAMk-clients"));
        // Already there (e.g. only read): no move.
        let read = Pending {
            folder: clients.clone(),
            ..pending(Place::Folder, Place::Folder, Some(5))
        };
        assert_eq!(move_destination(&read, None), None);
        // Done undone after it reached Archive: back into the folder.
        let undone = Pending {
            folder: clients,
            ..pending(Place::Folder, Place::Archive, None)
        };
        assert_eq!(move_destination(&undone, None), Some("AAMk-clients"));
        // Done, back to the inbox, and filed without a known folder.
        assert_eq!(
            move_destination(&pending(Place::Archive, Place::Folder, Some(5)), None),
            Some("archive")
        );
        assert_eq!(
            move_destination(&pending(Place::Inbox, Place::Folder, Some(5)), None),
            Some("inbox")
        );
        assert_eq!(
            move_destination(&pending(Place::Folder, Place::Folder, Some(5)), None),
            None
        );
        // Blocked mail goes to the Blocked folder, once it's known.
        let blocked = pending(Place::Blocked, Place::Inbox, None);
        assert_eq!(
            move_destination(&blocked, Some("AAMk-blocked")),
            Some("AAMk-blocked")
        );
        assert_eq!(move_destination(&blocked, None), None);
        let there = pending(Place::Blocked, Place::Blocked, None);
        assert_eq!(move_destination(&there, Some("AAMk-blocked")), None);
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
            ..Default::default()
        };
        assert_eq!(
            draft_message(&draft, &draft.bodies(None, Some(OUTLOOK_BODY))),
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

    /// A stand-in for Graph on a local port: answers each GET with
    /// `reply(path)` as JSON, or 404 for `None`, and records the paths.
    /// A local stand-in for Graph answering GETs and POSTs by path with
    /// `reply` (404 for `None`); returns its base URL and the paths asked for.
    pub(crate) async fn stand_in(
        reply: fn(&str) -> Option<Value>,
    ) -> (String, std::sync::Arc<Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut chunk).await {
                        Ok(n) if n > 0 => buf.extend_from_slice(&chunk[..n]),
                        _ => break,
                    }
                }
                let request = String::from_utf8_lossy(&buf);
                let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let (status, body) = match reply(&path) {
                    Some(json) => ("200 OK", json.to_string()),
                    None => (
                        "404 Not Found",
                        json!({ "error": { "message": "nope" } }).to_string(),
                    ),
                };
                log.lock().unwrap().push(path);
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        (base, seen)
    }

    fn attachments_of_m1(path: &str) -> Option<Value> {
        let png = "iVBORw0KGgo=";
        Some(match path.strip_prefix("/me/messages/m1/attachments")? {
            p if p.starts_with('?') => json!({ "value": [
                { "id": "file", "contentType": "application/pdf", "size": 9000, "isInline": false },
                { "id": "photo", "contentType": "image/jpeg", "size": 100, "isInline": false },
                { "id": "unused", "contentType": "image/gif", "size": 100, "isInline": true },
                { "id": "logo", "contentType": "image/png", "size": 100, "isInline": true },
            ] }),
            "/logo" => json!({ "name": "image001.png", "contentType": "image/png",
                               "contentId": "image001.png@01DD", "contentBytes": png }),
            "/unused" => {
                json!({ "contentType": "image/gif", "contentId": "<other@x>", "contentBytes": png })
            }
            "/photo" => json!({ "name": "photo.jpg", "contentType": "application/octet-stream",
                                "contentId": "Photo", "contentBytes": png }),
            _ => return None,
        })
    }

    #[tokio::test]
    async fn inline_images_come_from_the_inline_attachments_first() {
        let (base, seen) = stand_in(attachments_of_m1).await;
        let graph = Graph::with_base(&base, "token");

        let logo = graph
            .inline_images("m1", &["image001.png@01dd".to_owned()])
            .await
            .unwrap();
        assert_eq!(logo.len(), 1);
        assert_eq!(
            (logo[0].cid.as_str(), logo[0].mime.as_str()),
            ("image001.png@01dd", "image/png")
        );
        assert_eq!(logo[0].bytes, b"\x89PNG\r\n\x1a\n");
        let fetched: Vec<String> = seen.lock().unwrap().drain(..).collect();
        assert!(
            fetched[0].starts_with("/me/messages/m1/attachments?"),
            "{fetched:?}"
        );
        assert!(
            !fetched
                .iter()
                .any(|p| p.ends_with("/file") || p.ends_with("/photo")),
            "the PDF is never fetched, the non-inline image not while inline ones remain: {fetched:?}"
        );

        // A cid only a non-inline image has: found after the inline ones.
        let both = graph
            .inline_images("m1", &["image001.png@01dd".to_owned(), "photo".to_owned()])
            .await
            .unwrap();
        let cids: Vec<&str> = both.iter().map(|i| i.cid.as_str()).collect();
        assert_eq!(cids, ["image001.png@01dd", "photo"]);
        assert_eq!(both[1].mime, "image/jpeg", "typed by its name");
        assert!(!seen.lock().unwrap().iter().any(|p| p.ends_with("/file")));
    }

    /// 2026-10-21 14:00 UTC.
    const NEWEST: i64 = 1_792_591_200_000;

    /// When message `n` of a stand-in mailbox arrived: an hour apart, the
    /// first newest.
    fn received(n: usize) -> i64 {
        NEWEST - n as i64 * 3_600_000
    }

    /// A stand-in mailbox: 350 inbox messages `i0`… and 260 in folder `f1`
    /// (`m0`…), newest first. Listings take `$filter` (`lt` only), `$skip`
    /// and `$top`, give at most 100 a page, and link the next page.
    fn mailbox(path: &str) -> Option<Value> {
        let (route, query) = path.split_once('?').unwrap_or((path, ""));
        let q: HashMap<&str, &str> = query.split('&').filter_map(|p| p.split_once('=')).collect();
        let full = |id: &str, n: usize| {
            json!({
                "id": id, "subject": format!("Message {id}"), "isRead": true,
                "receivedDateTime": iso_date(received(n)),
                "from": { "emailAddress": { "name": "Ana", "address": "ana@contoso.com" } },
                "internetMessageId": format!("<{id}@contoso.com>"),
                "body": { "contentType": "text", "content": "Hi" },
            })
        };
        if let Some(id) = route.strip_prefix("/me/messages/") {
            return Some(full(id, id[1..].parse().ok()?));
        }
        let (prefix, count) = match route {
            "/me/mailFolders/inbox/messages" => ("i", 350),
            "/me/mailFolders/f1/messages" => ("m", 260),
            _ => return None,
        };
        let before = q
            .get("$filter")
            .and_then(|f| f.strip_prefix("receivedDateTime+lt+"))
            .and_then(parse_iso_date)
            .unwrap_or(i64::MAX);
        let matching: Vec<usize> = (0..count).filter(|n| received(*n) < before).collect();
        let skip: usize = q.get("$skip").map_or(0, |s| s.parse().unwrap());
        let top: usize = q["$top"].parse().unwrap();
        let page: Vec<usize> = matching
            .iter()
            .copied()
            .skip(skip)
            .take(top.min(100))
            .collect();
        let listing = q["$select"].starts_with("id,");
        let value: Vec<Value> = page
            .iter()
            .map(|n| {
                let id = format!("{prefix}{n}");
                if listing {
                    json!({ "id": id, "receivedDateTime": iso_date(received(*n)), "isRead": true })
                } else {
                    full(&id, *n)
                }
            })
            .collect();
        let mut reply = json!({ "value": value });
        if skip + page.len() < matching.len() {
            let rest: Vec<&str> = query
                .split('&')
                .filter(|p| !p.starts_with("$skip="))
                .collect();
            reply["@odata.nextLink"] = json!(format!(
                "{route}?{}&$skip={}",
                rest.join("&"),
                skip + page.len()
            ));
        }
        Some(reply)
    }

    fn microsoft_store() -> (Mutex<Store>, i64) {
        let store = Store::open_in_memory().unwrap();
        let account = store
            .add_account(&spark_store::NewAccount {
                kind: spark_store::AccountKind::Microsoft,
                email: "me@contoso.com",
                name: "",
                label: "Work",
                config: "{}",
            })
            .unwrap();
        (Mutex::new(store), account.id)
    }

    fn blocking(path: &str) -> Option<Value> {
        Some(match path {
            p if p.starts_with("/me/mailFolders?$filter=") => json!({ "value": [] }),
            "/me/mailFolders" => json!({ "id": "AAMk-blocked", "displayName": "Blocked" }),
            "/me/messages/m9/move" => json!({ "id": "m9" }),
            _ => return None,
        })
    }

    #[tokio::test]
    async fn blocked_mail_moves_to_a_blocked_folder_made_for_it() {
        let (store, account) = microsoft_store();
        let id = {
            let mut s = lock(&store);
            let incoming = Incoming {
                account_id: account,
                remote_id: Some("m9".into()),
                from_address: "spam@z.net".into(),
                date: 1,
                ..Default::default()
            };
            let Added::New(id) = s.add_incoming(&incoming, true).unwrap() else {
                panic!("stored before");
            };
            let block = spark_store::Patch {
                state: Some(spark_store::MessageState::Blocked),
                ..Default::default()
            };
            s.update(id, &block, 1).unwrap();
            id
        };
        let (base, seen) = stand_in(blocking).await;
        let graph = Graph::with_base(&base, "token");
        let pending = lock(&store).pending(account, 2, 0).unwrap();
        assert_eq!(graph.push(&store, pending).await.unwrap(), 1);
        let paths = seen.lock().unwrap().clone();
        assert!(
            paths[0].starts_with("/me/mailFolders?$filter="),
            "{paths:?}"
        );
        assert_eq!(&paths[1..], ["/me/mailFolders", "/me/messages/m9/move"]);
        let at = lock(&store).location(id).unwrap().unwrap();
        assert_eq!(at.remote_place, Place::Blocked);
        assert!(lock(&store).pending(account, 2, 0).unwrap().is_empty());
    }

    fn sent_items(path: &str) -> Option<Value> {
        path.starts_with("/me/mailFolders/sentitems/messages?")
            .then(|| {
                json!({ "value": [{ "toRecipients": [
                { "emailAddress": { "name": "Jo Ann", "address": "JoAnn@vendor.org" } }
            ], "ccRecipients": [] }] })
            })
    }

    #[tokio::test]
    async fn written_to_needs_the_exact_address_among_the_recipients() {
        let (base, _) = stand_in(sent_items).await;
        let graph = Graph::with_base(&base, "token");
        let senders = ["ann@vendor.org".to_owned(), "joann@vendor.org".to_owned()];
        assert_eq!(
            graph.written_to(&senders).await.unwrap(),
            ["joann@vendor.org"]
        );
    }

    /// The `$skip`s of the listings with bodies among `paths`.
    fn full_listings(paths: &[String]) -> Vec<usize> {
        paths
            .iter()
            .filter(|p| p.contains("messages?") && p.contains("$select=subject"))
            .map(|p| {
                let skip = p.split("$skip=").nth(1).expect("full listings skip");
                skip.split('&').next().unwrap().parse().unwrap()
            })
            .collect()
    }

    #[tokio::test]
    async fn folders_reach_back_by_raising_their_limit() {
        let (base, seen) = stand_in(mailbox).await;
        let graph = Graph::with_base(&base, "token");
        let (store, account_id) = microsoft_store();
        let folder = {
            let mut s = lock(&store);
            s.replace_folders(
                account_id,
                &[RemoteFolder {
                    remote_id: "f1".into(),
                    parent: None,
                    name: "Projects".into(),
                    role: None,
                    selectable: true,
                }],
            )
            .unwrap();
            s.folders().unwrap().remove(0)
        };

        // The newest 200, listed over two pages, then fetched in full.
        let mut state = FolderState::default();
        let changed = graph.sync_folder(&store, &folder, &mut state).await;
        assert_eq!(changed.unwrap(), Some(200));
        assert!(!state.complete, "60 more are on the server");
        let paths: Vec<String> = seen.lock().unwrap().drain(..).collect();
        assert!(paths[0].contains("$top=200"), "{paths:?}");
        assert_eq!(full_listings(&paths), [0, 50, 100, 150]);

        // 100 further back: the rest, and only the rest is fetched in full.
        state.limit = Some(FOLDER_LIMIT + OLDER_STEP);
        let changed = graph.sync_folder(&store, &folder, &mut state).await;
        assert_eq!(changed.unwrap(), Some(60));
        assert!(state.complete);
        let paths: Vec<String> = seen.lock().unwrap().drain(..).collect();
        assert_eq!(full_listings(&paths), [200, 250]);
        assert_eq!(
            lock(&store).folder_member_ids(folder.id).unwrap().len(),
            260
        );

        // Nothing new: listed, nothing fetched.
        let changed = graph.sync_folder(&store, &folder, &mut state).await;
        assert_eq!(changed.unwrap(), Some(0));
        assert!(full_listings(&seen.lock().unwrap()).is_empty());
    }

    #[tokio::test]
    async fn the_inbox_reaches_back_past_the_delta_window() {
        let (base, seen) = stand_in(mailbox).await;
        let graph = Graph::with_base(&base, "token");
        let (store, account_id) = microsoft_store();
        // An account whose delta window starts between i49 and i50, and
        // which already has i50…i79 (as if the window had been wider).
        let since = received(50) + 1_800_000;
        for n in 50..80 {
            let m = Incoming {
                account_id,
                remote_id: Some(format!("i{n}")),
                date: received(n),
                ..Default::default()
            };
            lock(&store).add_incoming(&m, false).unwrap();
        }
        let mut state = SyncState {
            delta_link: Some(format!("{base}/delta")),
            since: Some(since),
            ..Default::default()
        };

        // Past the stored ones until at least 100 are new: two pages.
        let stored = graph.older_inbox(&store, account_id, &mut state).await;
        assert_eq!(stored.unwrap(), 70 + 100);
        assert_eq!(state.older_than, Some(received(249)));
        assert!(!state.complete);
        let paths: Vec<String> = seen.lock().unwrap().drain(..).collect();
        assert!(
            paths[0].contains(&format!(
                "$filter=receivedDateTime+lt+{}&$orderby=receivedDateTime+desc",
                iso_date(since)
            )),
            "{paths:?}"
        );
        assert_eq!(
            full_listings(&paths),
            [30, 80, 100, 150],
            "from the first unknown, not the stored ones"
        );
        let messages = lock(&store).messages().unwrap();
        let old = messages
            .iter()
            .find(|m| m.subject == "Message i200")
            .unwrap();
        assert!(!old.screening && old.state == spark_store::MessageState::Inbox);

        // The next 100, which end the inbox; i249 is listed again, as mail
        // of its second might not have been.
        let stored = graph.older_inbox(&store, account_id, &mut state).await;
        assert_eq!(stored.unwrap(), 100);
        assert_eq!(state.older_than, Some(received(349)));
        assert!(state.complete);
        assert_eq!(lock(&store).messages().unwrap().len(), 300);

        // Before the first sync there's nothing to reach back from.
        let mut fresh = SyncState::default();
        let stored = graph.older_inbox(&store, account_id, &mut fresh).await;
        assert_eq!(stored.unwrap(), 0);
        assert_eq!(fresh, SyncState::default());
    }

    /// A stand-in mailbox for searches. "invoice" finds, newest first, `i3`
    /// (inbox), `k4` (stored already), `m5` (folder f1), `t6` (Deleted
    /// Items), `x7` (a folder not listed), `a8` (Archive) and `i20` (inbox);
    /// "many" finds `i20`…`i31`. The digits say how many hours old each is.
    fn searchable(path: &str) -> Option<Value> {
        let (route, query) = path.split_once('?').unwrap_or((path, ""));
        let full = |id: &str| {
            let n: usize = id[1..].parse().unwrap();
            json!({
                "id": id, "subject": format!("Invoice {id}"), "isRead": false,
                "receivedDateTime": iso_date(received(n)),
                "from": { "emailAddress": { "name": "Ana", "address": "ana@contoso.com" } },
                "body": { "contentType": "text", "content": "The invoice" },
            })
        };
        if route == "/me/mailFolders/inbox" {
            return Some(json!({ "id": "inbox-id" }));
        }
        if let Some(id) = route.strip_prefix("/me/messages/") {
            return Some(full(id));
        }
        let hits: Vec<(String, &str)> = if query.contains("invoice") {
            [
                ("i3", "inbox-id"),
                ("k4", "inbox-id"),
                ("m5", "f1"),
                ("t6", "trash"),
                ("x7", "elsewhere"),
                ("a8", "archive"),
                ("i20", "inbox-id"),
            ]
            .map(|(id, folder)| (id.to_owned(), folder))
            .to_vec()
        } else if query.contains("many") {
            (20..32).map(|n| (format!("i{n}"), "inbox-id")).collect()
        } else {
            Vec::new()
        };
        let listing = query.contains("parentFolderId");
        let value: Vec<Value> = hits
            .iter()
            .map(|(id, folder)| {
                let mut m = full(id);
                if listing {
                    m = json!({ "id": id, "receivedDateTime": m["receivedDateTime"] });
                }
                m["parentFolderId"] = json!(folder);
                m
            })
            .collect();
        (route == "/me/messages").then(|| json!({ "value": value }))
    }

    #[tokio::test]
    async fn searches_store_what_the_mailbox_finds_where_it_belongs() {
        let (base, seen) = stand_in(searchable).await;
        let graph = Graph::with_base(&base, "token");
        let (store, account_id) = microsoft_store();
        {
            let mut s = lock(&store);
            let folder = |remote_id: &str, role| RemoteFolder {
                remote_id: remote_id.into(),
                parent: None,
                name: remote_id.into(),
                role,
                selectable: true,
            };
            s.replace_folders(
                account_id,
                &[
                    folder("f1", None),
                    folder("archive", Some(FolderRole::Archive)),
                    folder("trash", Some(FolderRole::Trash)),
                ],
            )
            .unwrap();
            let stored = Incoming {
                account_id,
                remote_id: Some("k4".into()),
                ..Default::default()
            };
            s.add_incoming(&stored, false).unwrap();
        }
        // The delta query has the last 10 hours.
        let state = SyncState {
            delta_link: Some(format!("{base}/delta")),
            since: Some(received(10)),
            ..Default::default()
        };

        let query = Query::parse("Invoice from:ana@contoso.com");
        let stored = graph
            .search_server(&store, account_id, &state, &query)
            .await;
        assert_eq!(stored.unwrap(), 3);
        let paths: Vec<String> = seen.lock().unwrap().drain(..).collect();
        assert!(
            paths[1].starts_with(
                "/me/messages?$search=%22invoice+AND+from%3Aana%40contoso.com%22&$top=100&$select=id,"
            ),
            "{paths:?}"
        );
        let fetched: Vec<&str> = paths
            .iter()
            .filter_map(|p| p.strip_prefix("/me/messages/"))
            .map(|p| p.split('?').next().unwrap())
            .collect();
        assert_eq!(
            fetched,
            ["m5", "a8", "i20"],
            "one by one, only those stored"
        );

        let messages = lock(&store).messages().unwrap();
        let state_of = |subject: &str| {
            let m = messages.iter().find(|m| m.subject == subject).unwrap();
            (m.state, m.screening)
        };
        use spark_store::MessageState::{Done, Filed, Inbox};
        assert_eq!(state_of("Invoice i20"), (Inbox, false));
        assert_eq!(state_of("Invoice m5"), (Filed, false));
        assert_eq!(state_of("Invoice a8"), (Done, false));
        assert_eq!(messages.len(), 4);
        assert_eq!(lock(&store).search("invoice").unwrap().len(), 3);

        // Again: nothing new to fetch.
        let again = graph
            .search_server(&store, account_id, &state, &query)
            .await;
        assert_eq!(again.unwrap(), 0);
        let paths: Vec<String> = seen.lock().unwrap().drain(..).collect();
        assert_eq!(paths.len(), 2, "{paths:?}");

        // Many new ones come in one listing with bodies.
        let many = graph
            .search_server(&store, account_id, &state, &Query::parse("many"))
            .await;
        assert_eq!(many.unwrap(), 11, "i20 was stored already");
        let paths: Vec<String> = seen.lock().unwrap().drain(..).collect();
        assert_eq!(paths.len(), 3, "{paths:?}");
        assert!(paths[2].contains("$select=subject,"), "{paths:?}");

        // Filters alone, or before the first sync, ask nothing.
        let unread = Query::parse("is:unread");
        assert_eq!(
            graph
                .search_server(&store, account_id, &state, &unread)
                .await
                .unwrap(),
            0
        );
        let fresh = SyncState::default();
        assert_eq!(
            graph
                .search_server(&store, account_id, &fresh, &query)
                .await
                .unwrap(),
            0
        );
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn states_from_before_reaching_back_still_read() {
        let state: SyncState = serde_json::from_str(r#"{"deltaLink":"https://x"}"#).unwrap();
        assert!(state.since.is_none() && state.older_than.is_none() && !state.complete);
        let folder: FolderState = serde_json::from_str("{}").unwrap();
        assert_eq!(folder, FolderState::default());
    }

    #[test]
    fn reply_text_is_escaped() {
        assert_eq!(
            text_to_html("a < b\nc & d"),
            "<div style=\"font-family:Calibri,Arial,sans-serif;font-size:11pt\">a &lt; b<br>c &amp; d</div><br>"
        );
    }
}
