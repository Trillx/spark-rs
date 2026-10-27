//! Microsoft Graph for Microsoft 365 mailboxes: the inbox through delta
//! queries over its recent mail (older mail comes in as the user scrolls to
//! it), other folders' newest messages, flag and move changes, folder
//! management, and sending.
//!
//! Every request asks for immutable ids, so a message keeps its id when it
//! moves to Archive or Deleted Items and back.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use futures_util::stream::{FuturesUnordered, StreamExt};
use reqwest::header::RETRY_AFTER;
use reqwest::{Method, RequestBuilder, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use spark_store::{
    Added, Attachment, Batch, Category, Folder, FolderRole, Incoming, Pending, Place, Pushed,
    Query, RemoteFolder, Store,
};

use crate::attachment::{
    AttachmentData, INLINE_IMAGE_MAX, INLINE_TOTAL_MAX, InlineImage, cid_key, mime_for,
};
use crate::draft::{Bodies, Draft, recipients};
use crate::invite::Invite;
use crate::parse::{automated_sender, bare_id, html_to_text, snippet};
use crate::search::{self, SERVER_HITS};
use crate::trackers;
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
/// Most messages of a folder's listing stored in one transaction.
const STORE_BATCH: usize = 50;
/// Most requests a piece of work has out at once (`in_turns`): Exchange
/// throttles an app with more than four at a time on one mailbox.
const IN_FLIGHT: usize = 4;
/// The longest pause Graph may ask for that's waited out. While a longer
/// one lasts, requests fail as busy without going out, for a later round.
const LONGEST_PAUSE: Duration = Duration::from_secs(30);
/// How often a throttled request is tried again.
const THROTTLED_RETRIES: u32 = 3;
/// Most messages a round fetches what's missing of (`Graph::repair`).
const REPAIR_BATCH: usize = 20;
/// The longest a round spends fetching what's missing (`Graph::repair`).
/// What's under way then is cut off, and goes to the end of the list.
const REPAIR_TIME: Duration = Duration::from_secs(60);
/// The longest each of those requests may take once it's sent, its answer
/// included; waiting out the account's pause before doesn't count. A
/// message asks for its attachments and then its invite, so both fit in
/// `REPAIR_TIME`: one too slow fails as a try of its own, rather than being
/// cut off with the round every time.
const REPAIR_REQUEST_TIME: Duration = Duration::from_secs(25);
/// A message still missing its attachments or invite this long after it
/// was stored, and after `REPAIR_TRIES` failed tries of its own, is given
/// up on (`Graph::repair`).
pub const REPAIR_FOR_MS: i64 = 7 * DAY_MS;
/// See `REPAIR_FOR_MS`. Repairs come with the app's sweeps, five minutes
/// apart, so a computer that was off for a week still tries for most of an
/// hour before giving up.
pub const REPAIR_TRIES: u32 = 10;
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
    /// New mail stored without its attachments or invite, as Graph couldn't
    /// give them yet, in the order a round fetches them (`Graph::repair`).
    /// Rounds and searches add to it side by side, so it's only changed
    /// where it's stored, in one go (`change_repairs`): a copy read earlier
    /// isn't saved (`save_state`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repair: Vec<Repair>,
}

/// A new message stored without what Graph couldn't give yet, for a later
/// round to fetch (`Graph::repair`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Repair {
    /// Its Graph id.
    pub id: String,
    #[serde(flatten)]
    pub missing: Missing,
    /// When it was stored without it (Unix ms).
    pub since: i64,
    /// How many times since Graph failed to give it, of its own: not when
    /// it held off the whole account (`everyone`).
    #[serde(default)]
    pub tries: u32,
}

/// What a listing leaves out of a message that's to be fetched.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Missing {
    /// Its attachments.
    pub attachments: bool,
    /// The invite a meeting message carries.
    pub invite: bool,
}

impl Missing {
    /// What a listing leaves out of `m`.
    fn of(m: &GraphMessage) -> Self {
        Self {
            attachments: m.has_attachments == Some(true),
            invite: m
                .odata_type
                .as_deref()
                .is_some_and(|t| t.contains("eventMessage")),
        }
    }

    fn any(self) -> bool {
        self.attachments || self.invite
    }

    /// This, but for what `other` has.
    fn without(self, other: Self) -> Self {
        Self {
            attachments: self.attachments && !other.attachments,
            invite: self.invite && !other.invite,
        }
    }
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

/// A message's attachment listing (`attachment_listing`).
#[derive(Deserialize)]
struct Attachments {
    value: Vec<ListedAttachment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListedAttachment {
    id: Option<String>,
    name: Option<String>,
    content_type: Option<String>,
    size: Option<u64>,
    is_inline: Option<bool>,
}

impl Attachments {
    /// The attachments listed, not counting inline images.
    fn files(self) -> Vec<Attachment> {
        self.value
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
            .collect()
    }
}

/// What a delta page says about one message, ready to store.
enum Delta {
    Gone(String),
    /// `flags` is false when only its headers changed.
    Changed {
        incoming: Box<Incoming>,
        flags: bool,
    },
}

/// What Graph gave of what a listing leaves out of a message
/// (`Graph::parts`).
#[derive(Default)]
struct Parts {
    /// Its attachments, not counting inline images.
    attachments: Vec<Attachment>,
    /// The invite it carries, as JSON.
    invite: Option<String>,
    /// What Graph may give later.
    missing: Missing,
    /// Graph held off every request of the account then (`everyone`).
    everyone: bool,
}

/// Graph's answer to a request for part of a message (`Graph::part`).
enum Part {
    /// What it gave, or `None` if it won't.
    Settled(Option<Vec<u8>>),
    /// It may give it later. `everyone` when it held off every request of
    /// the account then, not just this one.
    Later { everyone: bool },
}

/// What an account's Graph clients learn for the ones after them: until
/// when Graph asked to be left alone. Its rounds and searches share it
/// (`Graph::keeping`), as each makes a client of its own.
#[derive(Default)]
pub struct Standing {
    /// Until when Graph asked to be left alone (`paused`).
    paused_until: Mutex<Option<tokio::time::Instant>>,
}

impl Standing {
    /// Waits out the pause Graph asked for, if any. While more than
    /// `LONGEST_PAUSE` of it is left, fails as busy at once instead: the
    /// work goes in a later round.
    pub async fn paused(&self) -> Result<()> {
        loop {
            let until = *lock(&self.paused_until);
            let now = tokio::time::Instant::now();
            match until {
                Some(until) if until > now + LONGEST_PAUSE => return Err(busy()),
                Some(until) if until > now => tokio::time::sleep_until(until).await,
                _ => return Ok(()),
            }
        }
    }

    /// Holds back requests for `pause` from now, unless they're held back
    /// longer already.
    fn pause(&self, pause: Duration) {
        let until = tokio::time::Instant::now() + pause;
        let mut paused = lock(&self.paused_until);
        if paused.is_none_or(|paused| paused < until) {
            *paused = Some(until);
        }
    }
}

pub struct Graph {
    token: String,
    base: String,
    standing: Arc<Standing>,
    /// `REPAIR_TIME`, shorter in tests.
    repair_time: Duration,
    /// `REPAIR_REQUEST_TIME`, shorter in tests.
    repair_request_time: Duration,
}

/// Why a request got no answer from Graph (`send_once`).
enum Unanswered {
    /// It was held back while Graph asks to be left alone.
    Held,
    /// It failed on the way.
    Failed(reqwest::Error),
}

impl From<Unanswered> for Error {
    fn from(unanswered: Unanswered) -> Self {
        match unanswered {
            Unanswered::Held => busy(),
            Unanswered::Failed(e) => Error::network(HOST, e),
        }
    }
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
            standing: Arc::default(),
            repair_time: REPAIR_TIME,
            repair_request_time: REPAIR_REQUEST_TIME,
        }
    }

    /// This client, keeping to what the account's earlier clients learned
    /// (`standing`) and adding to it.
    pub fn keeping(self, standing: Arc<Standing>) -> Self {
        Self { standing, ..self }
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

    /// Sends `request`, keeping to Graph's pause (`send_once`).
    pub(crate) async fn send(&self, request: RequestBuilder) -> Result<reqwest::Response> {
        let (response, _) = self.send_once(request, 0).await?;
        checked(response).await
    }

    pub(crate) async fn json<T: for<'de> Deserialize<'de>>(
        &self,
        request: RequestBuilder,
    ) -> Result<T> {
        json_of(self.send(request).await?).await
    }

    /// `send` for requests that go a few at a time (`in_turns`), tried
    /// again when Graph throttles them (`answered_patiently`).
    async fn send_patiently(
        &self,
        request: impl Fn() -> RequestBuilder,
    ) -> Result<reqwest::Response> {
        checked(self.answered_patiently(request).await?).await
    }

    /// Graph's answer to a request, whatever its status. One Graph
    /// throttles is tried again once the pause is over, unless the pause
    /// is longer than `LONGEST_PAUSE` or it was tried `THROTTLED_RETRIES`
    /// times already. `request` makes it anew for each try.
    async fn answered_patiently(
        &self,
        request: impl Fn() -> RequestBuilder,
    ) -> Result<reqwest::Response, Unanswered> {
        let mut retries = 0;
        loop {
            match self.send_once(request(), retries).await? {
                (_, Some(pause)) if retries < THROTTLED_RETRIES && pause <= LONGEST_PAUSE => {
                    retries += 1;
                }
                (response, _) => return Ok(response),
            }
        }
    }

    /// Sends `request` once Graph's pause is over (`Standing::paused`). A
    /// throttled answer comes with the pause it asks for (`throttled`),
    /// which every request of the account's then keeps to, whether or not
    /// this one is tried again.
    async fn send_once(
        &self,
        request: RequestBuilder,
        retries: u32,
    ) -> Result<(reqwest::Response, Option<Duration>), Unanswered> {
        if self.standing.paused().await.is_err() {
            return Err(Unanswered::Held);
        }
        let response = request.send().await.map_err(Unanswered::Failed)?;
        let pause = throttled(&response, retries);
        if let Some(pause) = pause {
            self.standing.pause(pause);
        }
        Ok((response, pause))
    }

    async fn json_patiently<T: for<'de> Deserialize<'de>>(
        &self,
        request: impl Fn() -> RequestBuilder,
    ) -> Result<T> {
        json_of(self.send_patiently(request).await?).await
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
            // New mail is completed (attachments, invites) before the page
            // is stored, in one transaction. What Graph can't give yet is
            // listed in it too, for a later round to fetch.
            let known: HashSet<String> = {
                let store = lock(store);
                let mut known = HashSet::new();
                for m in &page.value {
                    if m.removed.is_none()
                        && m.is_read.is_some()
                        && store.has_remote_id(account_id, &m.id)?
                    {
                        known.insert(m.id.clone());
                    }
                }
                known
            };
            let mut changes = Vec::with_capacity(page.value.len());
            for m in &page.value {
                missing.remove(&m.id);
                changes.push(match m.removed {
                    Some(_) => Delta::Gone(m.id.clone()),
                    None => Delta::Changed {
                        incoming: Box::new(to_incoming(account_id, m)),
                        // A header-only delta must not overwrite the stored flags.
                        flags: m.is_read.is_some(),
                    },
                });
            }
            let new = page
                .value
                .iter()
                .zip(&mut changes)
                .filter_map(|(m, change)| match change {
                    Delta::Changed {
                        incoming,
                        flags: true,
                    } if !known.contains(&m.id) => Some((m, &mut **incoming)),
                    _ => None,
                })
                .collect();
            let repairs = self.complete_all(new).await;
            lock(store).batch(|b| {
                for change in &changes {
                    match change {
                        Delta::Gone(remote_id) => {
                            report.gone += b.observe_gone_remote(account_id, remote_id)?;
                        }
                        Delta::Changed { incoming, flags } => {
                            report.headers += usize::from(b.observe_reply_to(incoming)?);
                            if *flags && let Added::New(id) = b.add_incoming(incoming, !first)? {
                                report.new.push(id);
                            }
                        }
                    }
                }
                listed_for_repair(b, account_id, repairs)
            })?;
            match (page.next_link, page.delta_link) {
                (Some(next), _) => url = next,
                (None, Some(delta)) => {
                    lock(store).batch(|b| {
                        for remote_id in &missing {
                            report.gone += b.observe_gone_remote(account_id, remote_id)?;
                        }
                        Ok(())
                    })?;
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
        let complete = ended && listing.len() <= limit;
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
        for chunk in listing.chunks(STORE_BATCH) {
            // Each with the message in full when it's new.
            let mut incomings: Vec<(Incoming, Option<&GraphMessage>)> =
                Vec::with_capacity(chunk.len());
            for listed in chunk {
                let unread = !listed.is_read.unwrap_or(false);
                let flagged = listed
                    .flag
                    .as_ref()
                    .is_some_and(|f| f.flag_status == "flagged");
                incomings.push(if known.contains(&listed.id) {
                    let incoming = Incoming {
                        account_id,
                        remote_id: Some(listed.id.clone()),
                        reply_to: listed
                            .reply_to
                            .as_ref()
                            .map(|recipients| recipients.iter().filter_map(display).collect()),
                        unread,
                        flagged,
                        ..Default::default()
                    };
                    (incoming, None)
                } else if let Some(m) = full.get(&listed.id) {
                    (to_incoming(account_id, m), Some(m))
                } else {
                    continue;
                });
            }
            let new = incomings
                .iter_mut()
                .filter_map(|(incoming, m)| Some(((*m)?, incoming)))
                .collect();
            // What Graph can't give yet is listed as the chunk is stored.
            let repairs = self.complete_all(new).await;
            lock(store).batch(|b| {
                for (incoming, _) in &incomings {
                    changed += usize::from(b.observe_reply_to(incoming)?);
                    let id = match b.add_folder_message(folder.id, incoming)? {
                        Added::New(id) | Added::Known(id) => id,
                    };
                    if !before.contains(&id) {
                        changed += 1;
                    }
                    members.push((id, None));
                }
                listed_for_repair(b, account_id, repairs)
            })?;
        }
        changed += lock(store).set_folder_members(folder.id, &members)?;
        state.complete = complete;
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
            let ids: Vec<&str> = listed
                .iter()
                .filter(|m| unknown(m))
                .map(|m| m.id.as_str())
                .collect();
            full = self.messages_in_full(&ids).await?;
        }
        Ok(full)
    }

    /// The messages with `ids`, with their bodies, a few at a time. Ones
    /// moved or deleted since they were listed are left out.
    async fn messages_in_full(&self, ids: &[&str]) -> Result<HashMap<String, GraphMessage>> {
        let mut full = HashMap::new();
        let mut failed = None;
        let fetches = ids.iter().map(|id| self.message_in_full(id)).collect();
        in_turns(fetches, |_, fetched| match fetched {
            Ok(m) => {
                full.extend(m.map(|m| (m.id.clone(), m)));
                true
            }
            Err(e) => {
                failed.get_or_insert(e);
                false
            }
        })
        .await;
        failed.map_or(Ok(full), Err)
    }

    /// A message with its body; `None` if it was moved or deleted since it
    /// was listed.
    async fn message_in_full(&self, id: &str) -> Result<Option<GraphMessage>> {
        let url = format!("/me/messages/{}?$select={SELECT}", path(id));
        match self
            .json_patiently(|| {
                self.request(Method::GET, &url)
                    .header("Prefer", "outlook.body-content-type=\"html\"")
            })
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
            let new: Vec<&GraphMessage> = page
                .value
                .iter()
                .filter_map(|listed| full.get(&listed.id))
                .collect();
            let mut incomings: Vec<Incoming> =
                new.iter().map(|m| to_incoming(account_id, m)).collect();
            // What Graph can't give yet is listed as the page is stored.
            let repairs = self
                .complete_all(new.into_iter().zip(&mut incomings).collect())
                .await;
            stored += lock(store).batch(|b| {
                let mut new = 0;
                for incoming in &incomings {
                    if let Added::New(_) = b.add_incoming(incoming, false)? {
                        new += 1;
                    }
                }
                listed_for_repair(b, account_id, repairs)?;
                Ok(new)
            })?;
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
    /// before the first sync. Attachments and invites Graph can't give yet
    /// are left to the account's rounds, as for syncs (`repair`).
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
        let mut listing: Page<Listed> = self
            .json(self.request(
                Method::GET,
                &format!("{found}&$select={LISTED},parentFolderId"),
            ))
            .await?;
        // Graph's search gives REST ids whatever `Prefer` asks for, and the
        // store has immutable ones: what it finds is looked up, fetched and
        // stored by those, or mail stored already would be stored again.
        let ids: Vec<&str> = listing.value.iter().map(|m| m.id.as_str()).collect();
        let immutable = self.immutable_ids(&ids).await?;
        listing.value.retain_mut(|m| match immutable.get(&m.id) {
            Some(id) => {
                m.id = id.clone();
                true
            }
            // Gone since it was found.
            None => false,
        });
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
            full.extend(page.value.into_iter().filter_map(|mut m| {
                m.id = immutable.get(&m.id)?.clone();
                Some((m.id.clone(), m))
            }));
        } else {
            let ids: Vec<&str> = wanted.iter().map(|(m, _)| m.id.as_str()).collect();
            full = self.messages_in_full(&ids).await?;
        }
        let mut new = Vec::new();
        let mut found = Vec::new();
        for (listed, folder) in wanted {
            if let Some(m) = full.get(&listed.id) {
                new.push(m);
                found.push((to_incoming(account_id, m), folder));
            }
        }
        // What Graph can't give yet is listed as it's stored, for the
        // account's rounds to fetch.
        let incomings = found.iter_mut().map(|(incoming, _)| incoming);
        let repairs = self
            .complete_all(new.into_iter().zip(incomings).collect())
            .await;
        Ok(lock(store).batch(|b| {
            let mut stored = 0;
            for (incoming, folder) in &found {
                let added = match folder {
                    None => b.add_incoming(incoming, false)?,
                    Some(folder) => b.add_folder_message(folder.id, incoming)?,
                };
                if let Added::New(_) = added {
                    stored += 1;
                }
            }
            listed_for_repair(b, account_id, repairs)?;
            Ok(stored)
        })?)
    }

    /// The immutable ids of the messages with REST ids `ids`, by REST id.
    /// Ones Graph can't translate, such as messages gone since, are left
    /// out. Graph refuses the lot if any is immutable already.
    async fn immutable_ids(&self, ids: &[&str]) -> Result<HashMap<String, String>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Translated {
            source_id: Option<String>,
            target_id: Option<String>,
        }
        let mut immutable = HashMap::new();
        // Graph translates up to 1,000 at a time.
        for chunk in ids.chunks(1000) {
            let page: Page<Translated> = self
                .json(
                    self.request(Method::POST, "/me/translateExchangeIds")
                        .json(&json!({
                            "inputIds": chunk,
                            "sourceIdType": "restId",
                            "targetIdType": "restImmutableEntryId",
                        })),
                )
                .await?;
            immutable.extend(
                page.value
                    .into_iter()
                    .filter_map(|t| Some((t.source_id?, t.target_id?))),
            );
        }
        Ok(immutable)
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

    /// Message `id`'s Outlook categories.
    /// Message `id`'s Outlook categories, with the version of the message
    /// they're from (its ETag) for `set_categories`.
    pub async fn categories(&self, id: &str) -> Result<(Vec<String>, Option<String>)> {
        #[derive(Deserialize)]
        struct Categories {
            #[serde(default)]
            categories: Vec<String>,
            #[serde(rename = "@odata.etag")]
            etag: Option<String>,
        }
        let url = format!("/me/messages/{}?$select=categories", path(id));
        let found: Categories = self
            .json_patiently(|| self.request(Method::GET, &url))
            .await?;
        Ok((found.categories, found.etag))
    }

    /// Gives message `id` these Outlook categories in place of its own, if
    /// it's still at `version`. Returns false when it changed meanwhile
    /// (another app added a category, say): read it again and redo.
    pub async fn set_categories(
        &self,
        id: &str,
        categories: &[String],
        version: Option<&str>,
    ) -> Result<bool> {
        let url = format!("/me/messages/{}", path(id));
        let body = json!({ "categories": categories });
        let response = self
            .answered_patiently(|| {
                let request = self.request(Method::PATCH, &url).json(&body);
                match version {
                    Some(version) => request.header("If-Match", version),
                    None => request,
                }
            })
            .await?;
        if response.status() == StatusCode::PRECONDITION_FAILED {
            return Ok(false);
        }
        checked(response).await?;
        Ok(true)
    }

    /// Deletes a folder with its subfolders (Outlook keeps them in Deleted Items).
    pub async fn delete_folder(&self, id: &str) -> Result<()> {
        self.send(self.request(Method::DELETE, &format!("/me/mailFolders/{}", path(id))))
            .await?;
        Ok(())
    }

    /// Of `senders`, the ones the user has written to: Sent Items has mail
    /// to them. Search matches more than the exact address, so what it finds
    /// is checked for it (`SENT_CANDIDATES` at most).
    pub async fn written_to(&self, senders: &[String]) -> Result<Vec<String>> {
        let mut found = Vec::new();
        for address in senders.iter().filter(|a| !a.contains(['"', '\\'])) {
            let search = format!("\"recipients:{address}\"");
            let mut next = Some(format!(
                "/me/mailFolders/sentitems/messages?$search={}&$top=250\
                 &$select=toRecipients,ccRecipients,bccRecipients",
                path(&search)
            ));
            let mut checked = 0;
            let mut sent = false;
            while let Some(url) = next.take()
                && !sent
                && checked < SENT_CANDIDATES
            {
                let page: Page<Value> = self.json(self.request(Method::GET, &url)).await?;
                checked += page.value.len();
                sent = page.value.iter().any(|m| addressed_to(m, address));
                next = page.next_link;
            }
            if sent {
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

    /// Completes new messages, each given with its message as listed, a
    /// few at a time (`parts`); one listed twice is completed once. What
    /// Graph can't give yet is left out, and returned for the caller to
    /// list as they're stored (`listed_for_repair`). Once Graph holds off
    /// the whole account no more start, and the rest are returned missing
    /// all of it.
    async fn complete_all(&self, new: Vec<(&GraphMessage, &mut Incoming)>) -> Vec<Repair> {
        let mut seen = HashSet::new();
        let new: Vec<(&GraphMessage, &mut Incoming)> = new
            .into_iter()
            .filter(|(m, _)| seen.insert(m.id.as_str()))
            .collect();
        let fetching = new
            .iter()
            .map(|(m, _)| self.parts(&m.id, Missing::of(m), None))
            .collect();
        let mut fetched: Vec<Option<Parts>> = new.iter().map(|_| None).collect();
        in_turns(fetching, |i, parts| {
            let more = !parts.everyone;
            fetched[i] = Some(parts);
            more
        })
        .await;
        let since = spark_store::now_ms();
        let mut repairs = Vec::new();
        for ((m, incoming), parts) in new.into_iter().zip(fetched) {
            let parts = parts.unwrap_or_else(|| Parts {
                missing: Missing::of(m),
                ..Default::default()
            });
            incoming.attachments = parts.attachments;
            incoming.invite = parts.invite;
            if parts.missing.any() {
                repairs.push(Repair {
                    id: m.id.clone(),
                    missing: parts.missing,
                    since,
                    tries: 0,
                });
            }
        }
        repairs
    }

    /// Fetches what a listing leaves out of message `id` that's `wanted`:
    /// its attachments, and the invite a meeting message carries. What
    /// Graph won't give is left out, such as an attachment deleted since
    /// the message was listed or a listing it can't read. What it may give
    /// later is `missing`; once it holds off the whole account, the rest
    /// isn't asked for. Each request may take `limit` (`part`).
    async fn parts(&self, id: &str, wanted: Missing, limit: Option<Duration>) -> Parts {
        let mut parts = Parts::default();
        if wanted.attachments {
            match self.part(&attachment_listing(id), limit).await {
                Part::Settled(listing) => {
                    parts.attachments = listing
                        .and_then(|l| serde_json::from_slice::<Attachments>(&l).ok())
                        .map_or_else(Vec::new, Attachments::files);
                }
                Part::Later { everyone } => {
                    parts.missing.attachments = true;
                    parts.everyone = everyone;
                }
            }
        }
        if wanted.invite && parts.everyone {
            parts.missing.invite = true;
        } else if wanted.invite {
            // Exchange keeps meeting messages as items of their own and
            // writes the calendar part out only in their MIME.
            let mime = format!("/me/messages/{}/$value", path(id));
            match self.part(&mime, limit).await {
                Part::Settled(raw) => {
                    parts.invite = raw
                        .and_then(|raw| Invite::from_raw(&raw))
                        .map(|i| i.to_json());
                }
                Part::Later { everyone } => {
                    parts.missing.invite = true;
                    parts.everyone = everyone;
                }
            }
        }
        parts
    }

    /// The body of Graph's answer to `url`, for `parts`: `None` if Graph
    /// won't give it. `Later` if it may when asked again (`everyone`,
    /// `later`), or the answer was cut off. Each try may take `limit` from
    /// when it's sent until its answer is in, else the client's limit: one
    /// that takes longer is cut off too.
    async fn part(&self, url: &str, limit: Option<Duration>) -> Part {
        let request = || {
            let request = self.request(Method::GET, url);
            match limit {
                Some(limit) => request.timeout(limit),
                None => request,
            }
        };
        let response = match self.answered_patiently(request).await {
            Ok(response) => response,
            Err(Unanswered::Held) => return Part::Later { everyone: true },
            Err(Unanswered::Failed(_)) => return Part::Later { everyone: false },
        };
        let status = response.status();
        match checked(response).await {
            Ok(response) => match response.bytes().await {
                Ok(body) => Part::Settled(Some(body.into())),
                Err(_) => Part::Later { everyone: false },
            },
            Err(_) if everyone(status) => Part::Later { everyone: true },
            Err(_) if later(status) => Part::Later { everyone: false },
            Err(_) => Part::Settled(None),
        }
    }

    /// Fetches what new mail was stored without, as Graph couldn't give it
    /// then (`SyncState::repair`): for the first `REPAIR_BATCH` messages
    /// listed, a few at a time, for `REPAIR_TIME` at most, keeping to the
    /// account's pause. Each request may take `REPAIR_REQUEST_TIME`. What
    /// comes is written into the stored message and taken off the list
    /// (`repaired`), as is what Graph won't give, and a message no longer
    /// stored. What may come later goes to the end of the list, for a later
    /// round, as does what's cut off when the time is up.
    ///
    /// Never fails, so a round can't for it. Returns how many stored
    /// messages changed. `now` is Unix ms.
    pub async fn repair(&self, store: &Mutex<Store>, account_id: i64, now: i64) -> usize {
        let mut due = Vec::new();
        {
            let mut s = lock(store);
            let Ok(Some(state)) = stored_state(&s, account_id) else {
                return 0;
            };
            let mut gone = HashSet::new();
            for r in state.repair {
                match s.has_remote_id(account_id, &r.id) {
                    Ok(false) => {
                        gone.insert(r.id);
                    }
                    _ if due.len() < REPAIR_BATCH => due.push(r),
                    _ => {}
                }
            }
            if !gone.is_empty() {
                // Tried again next round if this fails.
                let _ = s.batch(|b| {
                    change_repairs(b, account_id, |list| {
                        list.retain(|r| !gone.contains(&r.id));
                    })
                });
            }
        }
        let mut changed = 0;
        let end = tokio::time::Instant::now() + self.repair_time;
        let fetching = due
            .iter()
            .map(|r| async move {
                let limit = Some(self.repair_request_time);
                let fetched = tokio::time::timeout_at(end, self.parts(&r.id, r.missing, limit));
                // Cut off, it goes to the end of the list as when Graph holds
                // off the account, without counting as a try of its own: it
                // may have been waiting out the account's pause.
                fetched.await.unwrap_or_else(|_| Parts {
                    missing: r.missing,
                    everyone: true,
                    ..Default::default()
                })
            })
            .collect();
        in_turns(fetching, |i, parts| {
            let more = !parts.everyone && tokio::time::Instant::now() < end;
            let mut s = lock(store);
            changed += usize::from(repaired(&mut s, account_id, &due[i], parts, now));
            more
        })
        .await;
        changed
    }

    /// A message's MIME, as Outlook would export it.
    pub async fn raw(&self, id: &str) -> Result<Vec<u8>> {
        let response = self
            .send(self.request(Method::GET, &format!("/me/messages/{}/$value", path(id))))
            .await?;
        Ok(response
            .bytes()
            .await
            .map_err(|e| Error::network(HOST, e))?
            .to_vec())
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
        let url = attachment_listing(id);
        let listing: Attachments = self
            .json_patiently(|| self.request(Method::GET, &url))
            .await?;
        Ok(listing.files())
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
        self.attachment_files(id, &[(index, wanted.clone())])
            .await?
            .pop()
            .ok_or(Error::Gone)
    }

    /// `attachment` for each `(index, attachment)` in `wanted`, in that
    /// order: listing the message's attachments at most once, and
    /// downloading a few at a time (`in_turns`), patiently.
    pub async fn attachment_files(
        &self,
        id: &str,
        wanted: &[(usize, Attachment)],
    ) -> Result<Vec<AttachmentData>> {
        let listed = if wanted.iter().any(|(_, a)| a.remote_id.is_none()) {
            self.attachments(id).await?
        } else {
            Vec::new()
        };
        let mut urls = Vec::with_capacity(wanted.len());
        for (index, a) in wanted {
            let attachment_id = match &a.remote_id {
                Some(attachment_id) => attachment_id,
                None => listed
                    .get(*index)
                    .filter(|l| l.name == a.name)
                    .or_else(|| listed.iter().find(|l| l.name == a.name))
                    .and_then(|l| l.remote_id.as_ref())
                    .ok_or_else(|| {
                        Error::Server(format!("{} is no longer attached to the email.", a.name))
                    })?,
            };
            urls.push(format!(
                "/me/messages/{}/attachments/{}/$value",
                path(id),
                path(attachment_id)
            ));
        }
        let downloads = urls
            .iter()
            .map(|url| async move {
                let response = self
                    .send_patiently(|| self.request(Method::GET, url))
                    .await?;
                response.bytes().await.map_err(|e| Error::network(HOST, e))
            })
            .collect();
        let mut fetched: Vec<Option<Result<_>>> = wanted.iter().map(|_| None).collect();
        in_turns(downloads, |i, bytes| {
            let ok = bytes.is_ok();
            fetched[i] = Some(bytes);
            ok
        })
        .await;
        // After a failure the rest don't start: its error is the answer.
        if let Some(Err(e)) = fetched
            .iter_mut()
            .find_map(|f| f.take_if(|bytes| bytes.is_err()))
        {
            return Err(e);
        }
        wanted
            .iter()
            .zip(fetched)
            .map(|((_, a), bytes)| {
                Ok(AttachmentData {
                    name: a.name.clone(),
                    mime: if a.mime.is_empty() {
                        mime_for(&a.name).to_owned()
                    } else {
                        a.mime.clone()
                    },
                    bytes: bytes.ok_or(Error::Gone)??.to_vec(),
                })
            })
            .collect()
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
        let mut sendable = Vec::with_capacity(pending.len());
        for p in &pending {
            match &p.remote_id {
                Some(id) => sendable.push((id.as_str(), p)),
                None => lock(store).drop_pending(p.id)?,
            }
        }
        // A few messages at a time, each recorded as soon as it's through.
        // After a failure nothing more starts, and what's under way is
        // still recorded: the rest stays pending for the next round.
        let sending = sendable
            .iter()
            .map(|&(id, p)| self.push_one(id, p, blocked.as_deref()))
            .collect();
        let mut pushed = 0;
        let mut failed = None;
        in_turns(sending, |i, sent| {
            let (_, p) = sendable[i];
            let recorded = match sent {
                Ok(()) => {
                    let at = Pushed {
                        mailbox: None,
                        uid: None,
                    };
                    lock(store).mark_pushed(p, at).map(|()| pushed += 1)
                }
                Err(Error::Gone) => lock(store).drop_pending(p.id),
                Err(e) => {
                    failed.get_or_insert(e);
                    return false;
                }
            };
            recorded.map_err(|e| failed.get_or_insert(e.into())).is_ok()
        })
        .await;
        failed.map_or(Ok(pushed), Err)
    }

    /// Sends message `id`'s local changes `p`: its read and flagged state,
    /// then its move.
    async fn push_one(&self, id: &str, p: &Pending, blocked: Option<&str>) -> Result<()> {
        let message = format!("/me/messages/{}", path(id));
        if p.unread != p.remote_unread || p.pinned != p.remote_pinned {
            let flag = if p.pinned { "flagged" } else { "notFlagged" };
            let body = json!({ "isRead": !p.unread, "flag": { "flagStatus": flag } });
            self.send_patiently(|| self.request(Method::PATCH, &message).json(&body))
                .await?;
        }
        if let Some(destination) = move_destination(p, blocked) {
            let body = json!({ "destinationId": destination });
            let url = format!("{message}/move");
            self.send_patiently(|| self.request(Method::POST, &url).json(&body))
                .await?;
        }
        Ok(())
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
            "body": { "contentType": "HTML", "content": reply_html(&ours, &quoted) },
            "toRecipients": to, "ccRecipients": cc, "bccRecipients": bcc,
        });
        let draft_path = format!("/me/messages/{}", path(&reply.id));
        self.send(self.request(Method::PATCH, &draft_path).json(&update))
            .await?;
        self.attach_and_send(&draft_path, files, &bodies.images)
            .await?;
        Ok(reply.internet_message_id.as_deref().map(bare_id))
    }

    /// Adds `files` and the body's and signature's `images` to a draft and
    /// sends it.
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

    /// An image the HTML shows by its `cid:`.
    async fn attach_inline(&self, draft_path: &str, image: &InlineImage) -> Result<()> {
        let extension = image.mime.rsplit('/').next().unwrap_or("png");
        let name = format!("image.{extension}");
        self.upload(
            draft_path,
            &name,
            &image.mime,
            &image.bytes,
            Some(&image.cid),
        )
        .await
    }

    async fn attach(&self, draft_path: &str, f: &AttachmentData) -> Result<()> {
        self.upload(draft_path, &f.name, &f.mime, &f.bytes, None)
            .await
    }

    /// Attaches a file, in one request if it's small, else in an upload
    /// session; with `cid`, as an image the HTML shows.
    async fn upload(
        &self,
        draft_path: &str,
        name: &str,
        mime: &str,
        bytes: &[u8],
        cid: Option<&str>,
    ) -> Result<()> {
        let inline = |mut item: Value| {
            if let Some(cid) = cid {
                item["isInline"] = Value::Bool(true);
                item["contentId"] = Value::String(cid.to_owned());
            }
            item
        };
        if bytes.len() < INLINE_ATTACHMENT_MAX {
            self.send(
                self.request(Method::POST, &format!("{draft_path}/attachments"))
                    .json(&inline(json!({
                        "@odata.type": "#microsoft.graph.fileAttachment",
                        "name": name,
                        "contentType": mime,
                        "contentBytes": STANDARD.encode(bytes),
                    }))),
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
                    "AttachmentItem": inline(json!({
                        "attachmentType": "file",
                        "name": name,
                        "size": bytes.len(),
                        "contentType": mime,
                    }))
                })),
            )
            .await?;
        let total = bytes.len();
        for (i, chunk) in bytes.chunks(UPLOAD_CHUNK).enumerate() {
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
                    "Microsoft didn't accept {name} ({}).",
                    response.status()
                )));
            }
        }
        Ok(())
    }
}

/// How many Sent Items a search found are checked for the exact address, at most.
const SENT_CANDIDATES: usize = 1000;

/// Whether a listed message is to `address` (To, Cc or Bcc).
fn addressed_to(m: &Value, address: &str) -> bool {
    ["toRecipients", "ccRecipients", "bccRecipients"]
        .iter()
        .filter_map(|field| m[field].as_array())
        .flatten()
        .filter_map(|r| r["emailAddress"]["address"].as_str())
        .any(|a| a.eq_ignore_ascii_case(address))
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
    /// A draft that's gone is made anew, and so is one with embedded
    /// images: they're attached with the draft.
    pub async fn save_draft(
        &self,
        update: Option<&str>,
        draft: &Draft,
        files: &[AttachmentData],
    ) -> Result<(String, Option<String>)> {
        let bodies = draft.draft_bodies(Some(OUTLOOK_BODY));
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

/// A message body as Graph takes it: HTML with rich text or a signature,
/// else text.
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

/// A reply's body: ours, above the quote Graph made of the original, without
/// the original's tracking pixels (each reader of the reply would set them off).
fn reply_html(ours: &str, quoted: &str) -> String {
    format!("{ours}{}", trackers::strip(quoted))
}

/// Graph ids can contain `/`, `+` and `=`; immutable ids are URL-safe base64.
pub(crate) fn path(id: &str) -> String {
    url::form_urlencoded::byte_serialize(id.as_bytes()).collect()
}

/// Where message `id`'s attachments are listed (`Attachments`).
fn attachment_listing(id: &str) -> String {
    format!(
        "/me/messages/{}/attachments?$select=id,name,contentType,size,isInline",
        path(id)
    )
}

/// Saves the account's sync `state`, but for its repair list: that stays
/// as stored, as rounds and searches change it as they go
/// (`change_repairs`).
pub fn save_state(store: &Store, account_id: i64, state: &SyncState) -> Result<()> {
    let repair = match stored_state(store, account_id)? {
        Some(stored) => stored.repair,
        None => state.repair.clone(),
    };
    let state = SyncState {
        repair,
        ..state.clone()
    };
    store.set_sync_state(account_id, &state_json(&state))?;
    Ok(())
}

/// The account's sync state as stored; `None` if the account is gone, or
/// its state can't be read.
fn stored_state(store: &Store, account_id: i64) -> Result<Option<SyncState>> {
    Ok(store
        .account(account_id)?
        .and_then(|a| serde_json::from_str(&a.sync_state).ok()))
}

fn state_json(state: &SyncState) -> String {
    serde_json::to_string(state).expect("sync states serialize")
}

/// Changes the account's repair list (`SyncState::repair`) where it's
/// stored, in one go: read and written in batch `b`, under the store's
/// lock, so what another round or search changed meanwhile stays.
fn change_repairs(
    b: &Batch,
    account_id: i64,
    change: impl FnOnce(&mut Vec<Repair>),
) -> spark_store::Result<()> {
    let stored = b.sync_state(account_id)?;
    let Some(mut state) = stored.and_then(|json| serde_json::from_str::<SyncState>(&json).ok())
    else {
        return Ok(());
    };
    let before = state.repair.clone();
    change(&mut state.repair);
    if state.repair != before {
        b.set_sync_state(account_id, &state_json(&state))?;
    }
    Ok(())
}

/// Lists `repairs` (`Graph::complete_all`) for the account's rounds to
/// fetch, in the batch that stores their messages: a message is never
/// stored without what it's missing being listed, even if the app is
/// killed then. A message listed already is missing what it was, and
/// these too.
fn listed_for_repair(b: &Batch, account_id: i64, repairs: Vec<Repair>) -> spark_store::Result<()> {
    if repairs.is_empty() {
        return Ok(());
    }
    #[cfg(test)]
    if tests::LISTING_FAILS.take() {
        return Err(spark_store::Error::InvalidQuery);
    }
    change_repairs(b, account_id, |list| {
        for new in repairs {
            match list.iter_mut().find(|r| r.id == new.id) {
                Some(listed) => {
                    listed.missing.attachments |= new.missing.attachments;
                    listed.missing.invite |= new.missing.invite;
                }
                None => list.push(new),
            }
        }
    })
}

/// Writes what Graph gave (`parts`) of what listed message `asked` was
/// missing (`Graph::repair`) into the stored message, and takes it off the
/// account's repair list, or what came of it. Still missing something
/// Graph may give later, it goes to the end of the list; but failing a try
/// of its own for the `REPAIR_TRIES`th time, `REPAIR_FOR_MS` or more after
/// it was stored (`now`), it's given up on. Returns whether the stored
/// message changed.
fn repaired(store: &mut Store, account_id: i64, asked: &Repair, parts: Parts, now: i64) -> bool {
    let came = !parts.attachments.is_empty() || parts.invite.is_some();
    let stored = if came {
        match store.fill_in_remote(
            account_id,
            &asked.id,
            &parts.attachments,
            parts.invite.as_deref(),
        ) {
            Ok(stored) => stored,
            // Left on the list as it was, for the next round.
            Err(_) => return false,
        }
    } else {
        true
    };
    let settled = asked.missing.without(parts.missing);
    let change = |list: &mut Vec<Repair>| {
        let Some(at) = list.iter().position(|r| r.id == asked.id) else {
            return;
        };
        let mut r = list.remove(at);
        r.missing = r.missing.without(settled);
        if !stored || !r.missing.any() {
            return;
        }
        if !parts.missing.any() {
            // Listed for more since it was asked for.
            list.insert(at, r);
            return;
        }
        if !parts.everyone {
            r.tries = r.tries.saturating_add(1);
            if r.tries >= REPAIR_TRIES && now - r.since >= REPAIR_FOR_MS {
                eprintln!(
                    "spark-rs: account {account_id}: gave up on what Graph couldn't give of message {} after {} tries",
                    r.id, r.tries
                );
                return;
            }
        }
        list.push(r);
    };
    // Left as it was if this fails: what came is fetched again.
    let _ = store.batch(|b| change_repairs(b, account_id, change));
    came && stored
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

/// `response` if it succeeded, else what went wrong.
async fn checked(response: reqwest::Response) -> Result<reqwest::Response> {
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
        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => busy(),
        _ => Error::Server(format!("Microsoft Graph: {detail}")),
    })
}

/// What a throttled request fails with: it's tried again in a later round.
fn busy() -> Error {
    Error::network(HOST, "Microsoft is busy; trying again shortly")
}

/// Whether Graph refused a request with `status` as it refuses every other
/// one: it's throttling them or unavailable, or the sign-in ran out. Asked
/// again later, it may give what it refused.
fn everyone(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE | StatusCode::UNAUTHORIZED
    )
}

/// Whether Graph may give what it refused with `status` when asked again
/// later, besides when it's refusing `everyone`: it was failing or too
/// slow. Other refusals stand, such as for an attachment deleted since its
/// message was listed.
fn later(status: StatusCode) -> bool {
    status.is_server_error() || status == StatusCode::REQUEST_TIMEOUT
}

async fn json_of<T: for<'de> Deserialize<'de>>(response: reqwest::Response) -> Result<T> {
    response
        .json()
        .await
        .map_err(|e| Error::Server(format!("Microsoft Graph sent an unexpected reply: {e}")))
}

/// How long Graph asks to be left alone when `response` is throttled: its
/// Retry-After, else a pause that doubles with each of the `retries`.
/// `None` if it isn't throttled.
fn throttled(response: &reqwest::Response, retries: u32) -> Option<Duration> {
    if !matches!(
        response.status(),
        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
    ) {
        return None;
    }
    // Read as a u32, so adding the pause to now can't overflow.
    let pause = response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok()?.trim().parse::<u32>().ok())
        .map_or(Duration::from_secs(1 << retries), |s| {
            Duration::from_secs(s.into())
        });
    Some(pause)
}

/// Runs `work` at most `IN_FLIGHT` at a time, handing each result to
/// `done` with the work's position as it comes in, in whatever order.
/// Once `done` returns false no more work starts, but what's under way
/// finishes and is handed over.
///
/// The work comes ready made, as futures do nothing until they're polled:
/// a lazy iterator's closure held across an await would keep the caller's
/// future from being `Send`.
async fn in_turns<F: Future>(work: Vec<F>, mut done: impl FnMut(usize, F::Output) -> bool) {
    let mut waiting = work.into_iter().enumerate();
    let mut running = FuturesUnordered::new();
    let mut starting = true;
    loop {
        while starting && running.len() < IN_FLIGHT {
            match waiting.next() {
                Some((i, work)) => running.push(async move { (i, work.await) }),
                None => starting = false,
            }
        }
        match running.next().await {
            Some((i, output)) => starting &= done(i, output),
            None => return,
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::Cell;
    use std::ops::Range;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};

    use super::*;

    thread_local! {
        /// Fails the next listing for repair (`listed_for_repair`), as
        /// though the app were killed as it wrote it.
        pub(crate) static LISTING_FAILS: Cell<bool> = const { Cell::new(false) };
    }

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

    /// A local stand-in for Graph answering GETs and POSTs by path with
    /// `reply` (404 for `None`); returns its base URL and the paths asked for.
    pub(crate) async fn stand_in(
        reply: fn(&str) -> Option<Value>,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let paths = Arc::new(Mutex::new(Vec::new()));
        let log = paths.clone();
        let (base, _) = serve(move |asked, _| {
            log.lock().unwrap().push(asked.path.clone());
            match reply(&asked.path) {
                Some(json) => Answer::json(json),
                None => Answer::status(404),
            }
        })
        .await;
        (base, paths)
    }

    /// A request as a stand-in saw it.
    #[derive(Clone, Debug)]
    pub(crate) struct Asked {
        pub(crate) method: String,
        pub(crate) path: String,
        /// Its `Prefer` headers.
        pub(crate) prefer: Vec<String>,
        pub(crate) at: std::time::Instant,
        pub(crate) body: String,
        pub(crate) if_match: Option<String>,
    }

    /// A stand-in's answer: its status (0 drops the connection unanswered),
    /// Retry-After, body, and how long it takes.
    pub(crate) struct Answer {
        status: u16,
        retry_after: Option<u64>,
        body: String,
        delay: Duration,
    }

    impl Answer {
        pub(crate) fn json(body: Value) -> Self {
            Self {
                status: 200,
                retry_after: None,
                body: body.to_string(),
                delay: Duration::ZERO,
            }
        }

        pub(crate) fn status(status: u16) -> Self {
            Self {
                status,
                ..Self::json(json!({ "error": { "message": "nope" } }))
            }
        }

        fn after(self, delay: Duration) -> Self {
            Self { delay, ..self }
        }

        /// This answer, asking to be tried again after `seconds`.
        pub(crate) fn with_retry_after(self, seconds: u64) -> Self {
            Self {
                retry_after: Some(seconds),
                ..self
            }
        }
    }

    /// What a stand-in saw: the requests as they came in, and the most it
    /// was answering at once.
    #[derive(Default)]
    pub(crate) struct Seen {
        pub(crate) asked: Mutex<Vec<Asked>>,
        /// How often each method and path was asked for, ever.
        attempts: Mutex<HashMap<String, usize>>,
        answering: AtomicUsize,
        pub(crate) most: AtomicUsize,
    }

    impl Seen {
        /// The requests so far, forgotten.
        pub(crate) fn take(&self) -> Vec<Asked> {
            std::mem::take(&mut *self.asked.lock().unwrap())
        }
    }

    /// A local stand-in for Graph that serves connections side by side,
    /// answering each request with `answer(request, attempt)`; `attempt`
    /// counts the earlier requests with its method and path.
    pub(crate) async fn serve(
        answer: impl Fn(&Asked, usize) -> Answer + Send + Sync + 'static,
    ) -> (String, Arc<Seen>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Seen::default());
        let answer = Arc::new(answer);
        let log = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let (seen, answer) = (log.clone(), answer.clone());
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let end = loop {
                        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break at + 4;
                        }
                        match stream.read(&mut chunk).await {
                            Ok(n) if n > 0 => buf.extend_from_slice(&chunk[..n]),
                            _ => return,
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..end]).into_owned();
                    let mut lines = head.lines();
                    let mut first = lines.next().unwrap_or_default().split_whitespace();
                    let method = first.next().unwrap_or_default().to_owned();
                    let path = first.next().unwrap_or("/").to_owned();
                    let mut prefer = Vec::new();
                    let mut if_match = None;
                    let mut length = 0;
                    for (name, value) in lines.filter_map(|l| l.split_once(':')) {
                        if name.eq_ignore_ascii_case("prefer") {
                            prefer.push(value.trim().to_owned());
                        } else if name.eq_ignore_ascii_case("if-match") {
                            if_match = Some(value.trim().to_owned());
                        } else if name.eq_ignore_ascii_case("content-length") {
                            length = value.trim().parse().unwrap_or(0);
                        }
                    }
                    // The whole body, so closing doesn't reset the connection.
                    while buf.len() < end + length {
                        match stream.read(&mut chunk).await {
                            Ok(n) if n > 0 => buf.extend_from_slice(&chunk[..n]),
                            _ => return,
                        }
                    }
                    let asked = Asked {
                        method,
                        path,
                        prefer,
                        at: std::time::Instant::now(),
                        body: String::from_utf8_lossy(&buf[end..end + length]).into_owned(),
                        if_match,
                    };
                    let attempt = {
                        let mut attempts = seen.attempts.lock().unwrap();
                        let count = attempts
                            .entry(format!("{} {}", asked.method, asked.path))
                            .or_default();
                        *count += 1;
                        *count - 1
                    };
                    seen.asked.lock().unwrap().push(asked.clone());
                    let answering = seen.answering.fetch_add(1, SeqCst) + 1;
                    seen.most.fetch_max(answering, SeqCst);
                    let reply = answer(&asked, attempt);
                    tokio::time::sleep(reply.delay).await;
                    // Done before the client hears, so the count never runs ahead of it.
                    seen.answering.fetch_sub(1, SeqCst);
                    if reply.status != 0 {
                        let retry_after = reply
                            .retry_after
                            .map(|s| format!("Retry-After: {s}\r\n"))
                            .unwrap_or_default();
                        let response = format!(
                            "HTTP/1.1 {} Stand-in\r\nContent-Type: application/json\r\n{retry_after}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                            reply.status,
                            reply.body.len(),
                            reply.body
                        );
                        let _ = stream.write_all(response.as_bytes()).await;
                    }
                    let _ = stream.shutdown().await;
                });
            }
        });
        (base, seen)
    }

    /// Ends the pause Graph asked for of clients keeping to `standing` at
    /// `until`, as though Graph had asked for that.
    pub(crate) fn pause_until(standing: &Standing, until: Option<tokio::time::Instant>) {
        *lock(&standing.paused_until) = until;
    }

    /// Every request asked for immutable ids.
    fn immutable(asked: &[Asked]) -> bool {
        asked
            .iter()
            .all(|a| a.prefer.iter().any(|p| p == "IdType=\"ImmutableId\""))
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

    /// m1's files as a stand-in has them, each taking a while: a1 to a4,
    /// and a5 called old.pdf, listed by name. a3 is throttled once when
    /// `throttle` is set.
    fn files_of_m1(throttle: bool) -> impl Fn(&Asked, usize) -> Answer {
        move |asked, attempt| {
            let Some(rest) = asked.path.strip_prefix("/me/messages/m1/attachments") else {
                return Answer::status(404);
            };
            let answer = match rest {
                r if r.starts_with("?$select=id,name") => Answer::json(json!({ "value": [
                    { "id": "a5", "name": "old.pdf", "contentType": "application/pdf", "size": 5 },
                ] })),
                "/a3/$value" if throttle && attempt == 0 => Answer {
                    retry_after: Some(1),
                    ..Answer::status(429)
                },
                r => match r.strip_prefix("/a").and_then(|r| r.strip_suffix("/$value")) {
                    Some(n) if ("1"..="5").contains(&n) => Answer {
                        body: format!("file {n}"),
                        ..Answer::json(Value::Null)
                    },
                    _ => Answer::status(404),
                },
            };
            answer.after(Duration::from_millis(25))
        }
    }

    fn m1_files() -> Vec<(usize, Attachment)> {
        let file = |name: &str, id: Option<&str>| Attachment {
            name: name.into(),
            mime: String::new(),
            size: 6,
            remote_id: id.map(str::to_owned),
        };
        vec![
            (0, file("one.pdf", Some("a1"))),
            (1, file("two.pdf", Some("a2"))),
            (2, file("three.pdf", Some("a3"))),
            (3, file("four.pdf", Some("a4"))),
            (4, file("old.pdf", None)),
        ]
    }

    #[tokio::test]
    async fn a_messages_files_download_a_few_at_a_time_and_wait_out_throttling() {
        let (base, seen) = serve(files_of_m1(false)).await;
        let graph = Graph::with_base(&base, "token");
        let wanted = m1_files();
        // One by one, as Save All went, once the HTTP client is set up.
        for (i, a) in &wanted {
            graph.attachment("m1", *i, a).await.unwrap();
        }
        seen.take();
        let started = std::time::Instant::now();
        for (i, a) in &wanted {
            graph.attachment("m1", *i, a).await.unwrap();
        }
        let one_by_one = started.elapsed();
        assert_eq!(seen.take().len(), 6);
        let started = std::time::Instant::now();
        let files = graph.attachment_files("m1", &wanted).await.unwrap();
        let elapsed = started.elapsed();
        let asked = seen.take();
        let most = seen.most.load(SeqCst);
        eprintln!(
            "m1's five files: {} requests, at most {most} at once, {elapsed:?}; one by one {one_by_one:?}",
            asked.len()
        );
        let got: Vec<(&str, &str, &[u8])> = files
            .iter()
            .map(|f| (f.name.as_str(), f.mime.as_str(), f.bytes.as_slice()))
            .collect();
        assert_eq!(
            got,
            [
                ("one.pdf", "application/pdf", &b"file 1"[..]),
                ("two.pdf", "application/pdf", b"file 2"),
                ("three.pdf", "application/pdf", b"file 3"),
                ("four.pdf", "application/pdf", b"file 4"),
                ("old.pdf", "application/pdf", b"file 5"),
            ]
        );
        assert_eq!(asked.len(), 6, "listed once, for old.pdf, then the five");
        assert_eq!(most, 4, "never more than four at once");
        assert!(immutable(&asked));

        // Throttled once, a3 waits and is asked again; the others finish.
        let (base, seen) = serve(files_of_m1(true)).await;
        let graph = Graph::with_base(&base, "token");
        let files = graph.attachment_files("m1", &wanted).await.unwrap();
        assert_eq!(files[2].bytes, b"file 3");
        let asked = seen.take();
        let a3 = asked.iter().filter(|a| a.path.contains("/a3/")).count();
        assert_eq!((asked.len(), a3), (7, 2));

        // One that's gone fails the lot.
        let mut gone = wanted.clone();
        gone[1].1.remote_id = Some("a9".into());
        assert!(graph.attachment_files("m1", &gone).await.is_err());
        let mut renamed = wanted;
        renamed[4].1.name = "new.pdf".into();
        assert!(graph.attachment_files("m1", &renamed).await.is_err());
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

    /// A search that matches parts of addresses: the mail to Ann is on the
    /// second page, after mail to Jo Ann.
    fn sent_items(path: &str) -> Option<Value> {
        let to =
            |address: &str| json!({ "toRecipients": [{ "emailAddress": { "address": address } }] });
        if path.starts_with("/me/mailFolders/sentitems/messages?$search=") {
            Some(json!({
                "value": [to("JoAnn@vendor.org")],
                "@odata.nextLink": "/me/mailFolders/sentitems/messages?page=2",
            }))
        } else if path == "/me/mailFolders/sentitems/messages?page=2" {
            Some(json!({ "value": [to("ann@vendor.org")] }))
        } else {
            None
        }
    }

    #[tokio::test]
    async fn written_to_needs_the_exact_address_among_the_recipients() {
        let (base, _) = stand_in(sent_items).await;
        let graph = Graph::with_base(&base, "token");
        let senders = ["ann@vendor.org".to_owned(), "joann@vendor.org".to_owned()];
        assert_eq!(graph.written_to(&senders).await.unwrap(), senders);
        // Without the second page, only Jo Ann.
        let (base, _) = stand_in(|path| {
            sent_items(path).map(|mut page| {
                page.as_object_mut().unwrap().remove("@odata.nextLink");
                page
            })
        })
        .await;
        let first_page = Graph::with_base(&base, "token");
        assert_eq!(
            first_page.written_to(&senders).await.unwrap(),
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

    /// A stand-in delta query of two pages. The first has new mail (`n1`),
    /// a stored message removed (`s1`) and one whose Reply-To alone changed
    /// (`s2`); the second new mail with an attachment (`n2`) and a stored
    /// message read elsewhere (`s3`), which has one too.
    fn delta(path: &str) -> Option<Value> {
        let full = |id: &str, read: bool| {
            json!({
                "id": id, "subject": format!("Message {id}"), "isRead": read,
                "receivedDateTime": iso_date(NEWEST), "hasAttachments": id != "n1",
                "from": { "emailAddress": { "name": "Ana", "address": "ana@contoso.com" } },
                "body": { "contentType": "text", "content": "Hi" },
            })
        };
        Some(
            match path.split_once('?').map_or(path, |(route, _)| route) {
                "/delta1" => json!({
                    "value": [
                        full("n1", false),
                        { "id": "s1", "@removed": { "reason": "deleted" } },
                        { "id": "s2", "replyTo": [
                            { "emailAddress": { "name": "Desk", "address": "desk@contoso.com" } }
                        ] },
                    ],
                    "@odata.nextLink": "/delta2",
                }),
                "/delta2" => json!({
                    "value": [full("n2", false), full("s3", true)],
                    "@odata.deltaLink": "/delta3",
                }),
                "/me/messages/n2/attachments" => json!({ "value": [
                { "id": "a1", "name": "plan.pdf", "contentType": "application/pdf", "size": 10 }
            ] }),
                _ => return None,
            },
        )
    }

    #[tokio::test]
    async fn the_inbox_delta_is_stored_a_page_at_a_time() {
        let (base, seen) = stand_in(delta).await;
        let graph = Graph::with_base(&base, "token");
        let (store, account_id) = microsoft_store();
        let stored: Vec<i64> = ["s1", "s2", "s3"]
            .into_iter()
            .map(|id| {
                let m = Incoming {
                    account_id,
                    remote_id: Some(id.into()),
                    from_address: "ana@contoso.com".into(),
                    unread: true,
                    date: NEWEST,
                    ..Default::default()
                };
                match lock(&store).add_incoming(&m, false).unwrap() {
                    Added::New(id) | Added::Known(id) => id,
                }
            })
            .collect();
        let mut state = SyncState {
            delta_link: Some("/delta1".into()),
            reply_to_selected: true,
            ..Default::default()
        };

        let report = graph.sync_inbox(&store, account_id, &mut state).await;
        let report = report.unwrap();
        assert_eq!(report.new.len(), 2);
        assert_eq!((report.gone, report.headers), (1, 1));
        assert_eq!(state.delta_link.as_deref(), Some("/delta3"));
        let s = lock(&store);
        let message = |id| s.message(id).unwrap().unwrap();
        assert_eq!(message(stored[0]).state, spark_store::MessageState::Done);
        assert_eq!(
            s.content(stored[1]).unwrap().unwrap().reply_to,
            ["Desk <desk@contoso.com>"]
        );
        assert!(message(stored[1]).unread, "only its headers changed");
        assert!(!message(stored[2]).unread);
        let n2 = s
            .messages()
            .unwrap()
            .into_iter()
            .find(|m| m.subject == "Message n2")
            .unwrap();
        assert_eq!(n2.attachment.as_deref(), Some("plan.pdf"));
        // Only new mail is completed: s3's attachments aren't asked for.
        let paths = seen.lock().unwrap().clone();
        let asked: Vec<&String> = paths
            .iter()
            .filter(|p| p.contains("/attachments"))
            .collect();
        assert_eq!(asked.len(), 1, "{paths:?}");
        assert!(asked[0].starts_with("/me/messages/n2/"));
    }

    /// A stand-in mailbox for searches. "invoice" finds, newest first, `i3`
    /// (inbox), `k4` (stored already), `m5` (folder f1), `t6` (Deleted
    /// Items), `x7` (a folder not listed), `a8` (Archive) and `i20` (inbox);
    /// "many" finds `i20`…`i31`. The digits say how many hours old each is.
    /// As in Graph, a search gives REST ids (`rest-i3`) whatever `Prefer`
    /// asks for, a message asked for by one keeps it, and
    /// `translateExchangeIds` gives the immutable ids (`i3`) the rest of
    /// the mailbox uses.
    fn searchable(asked: &Asked) -> Option<Value> {
        let path = asked.path.as_str();
        let (route, query) = path.split_once('?').unwrap_or((path, ""));
        let full = |id: &str| {
            let bare = id.strip_prefix("rest-").unwrap_or(id);
            let n: usize = bare[1..].parse().unwrap();
            json!({
                "id": id, "subject": format!("Invoice {bare}"), "isRead": false,
                "receivedDateTime": iso_date(received(n)),
                "from": { "emailAddress": { "name": "Ana", "address": "ana@contoso.com" } },
                "body": { "contentType": "text", "content": "The invoice" },
            })
        };
        if route == "/me/mailFolders/inbox" {
            return Some(json!({ "id": "inbox-id" }));
        }
        if route == "/me/translateExchangeIds" {
            let asked: Value = serde_json::from_str(&asked.body).unwrap();
            assert_eq!(asked["sourceIdType"], "restId");
            assert_eq!(asked["targetIdType"], "restImmutableEntryId");
            let value: Vec<Value> = asked["inputIds"]
                .as_array()
                .unwrap()
                .iter()
                .map(|id| {
                    let id = id.as_str().unwrap();
                    let immutable = id.strip_prefix("rest-").expect("only REST ids translate");
                    json!({ "sourceId": id, "targetId": immutable })
                })
                .collect();
            return Some(json!({ "value": value }));
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
                let mut m = full(&format!("rest-{id}"));
                if listing {
                    m = json!({ "id": m["id"], "receivedDateTime": m["receivedDateTime"] });
                }
                m["parentFolderId"] = json!(folder);
                m
            })
            .collect();
        (route == "/me/messages").then(|| json!({ "value": value }))
    }

    /// A store with the `searchable` stand-in's folders f1, Archive and
    /// Deleted Items, and k4.
    fn searched_store() -> (Mutex<Store>, i64) {
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
        (store, account_id)
    }

    /// A synced account whose delta query has the last 10 hours.
    fn searchable_state(base: &str) -> SyncState {
        SyncState {
            delta_link: Some(format!("{base}/delta")),
            since: Some(received(10)),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn searches_store_what_the_mailbox_finds_where_it_belongs() {
        let (base, seen) = serve(searched).await;
        let paths_asked = || -> Vec<String> { seen.take().into_iter().map(|a| a.path).collect() };
        let graph = Graph::with_base(&base, "token");
        let (store, account_id) = searched_store();
        let state = searchable_state(&base);

        let query = Query::parse("Invoice from:ana@contoso.com");
        let stored = graph
            .search_server(&store, account_id, &state, &query)
            .await;
        assert_eq!(stored.unwrap(), 3);
        let paths = paths_asked();
        assert!(
            paths[1].starts_with(
                "/me/messages?$search=%22invoice+AND+from%3Aana%40contoso.com%22&$top=100&$select=id,"
            ),
            "{paths:?}"
        );
        let mut fetched: Vec<&str> = paths
            .iter()
            .filter_map(|p| p.strip_prefix("/me/messages/"))
            .map(|p| p.split('?').next().unwrap())
            .collect();
        fetched.sort_unstable();
        assert_eq!(
            fetched,
            ["a8", "i20", "m5"],
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
        assert_eq!(remote_ids(&store), ["a8", "i20", "k4", "m5"]);

        // Again: nothing new to fetch.
        let again = graph
            .search_server(&store, account_id, &state, &query)
            .await;
        assert_eq!(again.unwrap(), 0);
        let paths = paths_asked();
        assert_eq!(paths.len(), 3, "{paths:?}");

        // Many new ones come in one listing with bodies.
        let many = graph
            .search_server(&store, account_id, &state, &Query::parse("many"))
            .await;
        assert_eq!(many.unwrap(), 11, "i20 was stored already");
        let paths = paths_asked();
        assert_eq!(paths.len(), 4, "{paths:?}");
        assert!(paths[3].contains("$select=subject,"), "{paths:?}");
        assert!(remote_ids(&store).iter().all(|id| !id.starts_with("rest-")));

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
        assert!(paths_asked().is_empty());
    }

    #[tokio::test]
    async fn a_search_doesnt_store_again_what_a_sync_stored() {
        let (base, _) = serve(searched).await;
        let graph = Graph::with_base(&base, "token");
        let (store, account_id) = searched_store();
        let state = searchable_state(&base);
        // m5, as its folder's sync stores it: by its immutable id.
        {
            let mut s = lock(&store);
            let f1 = s
                .folders()
                .unwrap()
                .into_iter()
                .find(|f| f.remote_id == "f1")
                .unwrap();
            let m5 = Incoming {
                account_id,
                remote_id: Some("m5".into()),
                subject: "Invoice m5".into(),
                date: received(5),
                ..Default::default()
            };
            s.add_folder_message(f1.id, &m5).unwrap();
        }

        let stored = graph
            .search_server(&store, account_id, &state, &Query::parse("invoice"))
            .await;
        assert_eq!(stored.unwrap(), 2, "a8 and i20");
        assert_eq!(remote_ids(&store), ["a8", "i20", "k4", "m5"]);
    }

    /// The `searchable` stand-in as `serve` asks it.
    fn searched(asked: &Asked, _: usize) -> Answer {
        searchable(asked).map_or_else(|| Answer::status(404), Answer::json)
    }

    /// The remote ids of the stored messages, sorted.
    fn remote_ids(store: &Mutex<Store>) -> Vec<String> {
        let s = lock(store);
        let mut ids: Vec<String> = s
            .messages()
            .unwrap()
            .iter()
            .filter_map(|m| s.location(m.id).unwrap()?.remote_id)
            .collect();
        ids.sort_unstable();
        ids
    }

    #[test]
    fn states_from_before_reaching_back_still_read() {
        let state: SyncState = serde_json::from_str(r#"{"deltaLink":"https://x"}"#).unwrap();
        assert!(state.since.is_none() && state.older_than.is_none() && !state.complete);
        assert!(state.repair.is_empty());
        let folder: FolderState = serde_json::from_str("{}").unwrap();
        assert_eq!(folder, FolderState::default());
    }

    #[test]
    fn folder_states_from_the_dropped_folder_delta_still_read() {
        let json = r#"{"limit":300,"complete":true,"deltaLink":"https://x","floor":1,"grown":2}"#;
        let folder: FolderState = serde_json::from_str(json).unwrap();
        let expected = FolderState {
            limit: Some(300),
            complete: true,
        };
        assert_eq!(folder, expected);
    }

    #[test]
    fn replies_leave_out_the_original_s_tracking_pixels() {
        let quoted = r#"<html><body><div id="divRplyFwdMsg">From: Ana</div><p>Sale!</p><img src="https://track.example/open.gif" width="1" height="1"></body></html>"#;
        let html = reply_html("<div>Thanks</div>", quoted);
        assert!(html.starts_with("<div>Thanks</div>"), "{html}");
        assert!(
            html.contains("<p>Sale!</p>") && !html.contains("open.gif"),
            "{html}"
        );
    }

    #[test]
    fn reply_text_is_escaped() {
        assert_eq!(
            text_to_html("a < b\nc & d"),
            "<div style=\"font-family:Calibri,Arial,sans-serif;font-size:11pt\">a &lt; b<br>c &amp; d</div><br>"
        );
    }

    /// The store's folder for the stand-in mailbox's `f1`.
    fn projects(store: &Mutex<Store>, account_id: i64) -> Folder {
        let mut s = lock(store);
        let f1 = RemoteFolder {
            remote_id: "f1".into(),
            parent: None,
            name: "Projects".into(),
            role: None,
            selectable: true,
        };
        s.replace_folders(account_id, &[f1]).unwrap();
        s.folders().unwrap().remove(0)
    }

    #[test]
    fn graph_work_can_move_between_threads() {
        // The app's runtime moves tasks between threads, so their futures
        // must be `Send`, whatever they hold across a request.
        fn send(_: impl Future + Send) {}
        let graph = Graph::new("token");
        let (store, account_id) = microsoft_store();
        let folder = projects(&store, account_id);
        let mut state = SyncState::default();
        send(graph.push(&store, Vec::new()));
        send(graph.sync_inbox(&store, account_id, &mut state));
        send(graph.older_inbox(&store, account_id, &mut state));
        send(graph.search_server(&store, account_id, &state, &Query::parse("x")));
        send(graph.sync_folder(&store, &folder, &mut FolderState::default()));
        send(graph.repair(&store, account_id, 0));
    }

    /// The number in a stand-in message id such as `m13`.
    fn number(id: &str) -> usize {
        id[1..].parse().unwrap()
    }

    /// A meeting request's MIME, as Graph gives it.
    const MEETING: &str = "Content-Type: text/calendar; method=REQUEST\r\n\r\n\
BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:sync\r\nSUMMARY:Sync\r\n\
DTSTART:20261103T120000Z\r\nDTEND:20261103T130000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    /// The `mailbox` stand-in where each message has an attachment named
    /// for it (`m7.pdf`) and every 25th is a meeting request. Answers about
    /// every third message take longer, so they come back out of order.
    /// m13's attachments are throttled for a second once, m40's are gone
    /// and m41's are listed unreadably. Asked for alone, m3 is gone and m5
    /// fails once.
    fn attached(asked: &Asked, attempt: usize) -> Answer {
        let route = asked.path.split('?').next().unwrap();
        let Some(rest) = route.strip_prefix("/me/messages/") else {
            let Some(mut listed) = mailbox(&asked.path) else {
                return Answer::status(404);
            };
            for m in listed["value"].as_array_mut().unwrap() {
                m["hasAttachments"] = json!(true);
                if number(m["id"].as_str().unwrap()).is_multiple_of(25) {
                    m["@odata.type"] = json!("#microsoft.graph.eventMessageRequest");
                }
            }
            return Answer::json(listed).after(Duration::from_millis(10));
        };
        let (id, part) = rest.split_once('/').unwrap_or((rest, ""));
        let answer = match (part, number(id)) {
            ("attachments", 13) if attempt == 0 => {
                return Answer {
                    retry_after: Some(1),
                    ..Answer::status(429)
                };
            }
            ("attachments", 40) | ("", 3) => Answer::status(404),
            ("attachments", 41) => Answer {
                body: "{\"value\": [".into(),
                ..Answer::json(Value::Null)
            },
            ("", 5) if attempt == 0 => Answer::status(500),
            ("attachments", _) => Answer::json(json!({ "value": [
                { "id": "a1", "name": format!("{id}.pdf"), "contentType": "application/pdf", "size": 10 }
            ] })),
            ("$value", _) => Answer {
                body: MEETING.into(),
                ..Answer::json(Value::Null)
            },
            ("", n) => {
                let mut m = mailbox(route).unwrap();
                m["hasAttachments"] = json!(true);
                if n.is_multiple_of(25) {
                    m["@odata.type"] = json!("#microsoft.graph.eventMessageRequest");
                }
                Answer::json(m)
            }
            _ => Answer::status(404),
        };
        let slow = number(id).is_multiple_of(3);
        answer.after(Duration::from_millis(if slow { 25 } else { 10 }))
    }

    /// The `n` of each of `asked` for attachments or MIME.
    fn completing(asked: &[Asked], part: &str) -> Vec<usize> {
        asked
            .iter()
            .filter_map(|a| {
                let rest = a.path.strip_prefix("/me/messages/")?;
                let (id, rest) = rest.split_once('/')?;
                rest.starts_with(part).then(|| number(id))
            })
            .collect()
    }

    #[tokio::test]
    async fn new_mail_is_completed_a_few_requests_at_a_time() {
        let (base, seen) = serve(attached).await;
        let graph = Graph::with_base(&base, "token");
        let (store, account_id) = microsoft_store();
        let folder = projects(&store, account_id);

        let started = std::time::Instant::now();
        let changed = graph
            .sync_folder(&store, &folder, &mut FolderState::default())
            .await;
        let elapsed = started.elapsed();
        let asked = seen.take();
        let most = seen.most.load(SeqCst);
        eprintln!(
            "200 new messages with attachments: {} requests, {} for attachments, at most {most} at once, {elapsed:?}",
            asked.len(),
            completing(&asked, "attachments").len()
        );
        assert_eq!(changed.unwrap(), Some(200));
        // Two listings, four with bodies, 200 attachment listings (m13's
        // twice) and eight meetings' MIME.
        assert_eq!(asked.len(), 2 + 4 + 201 + 8);
        assert_eq!(most, 4, "never more than four at once");
        assert!(immutable(&asked));
        let mut meetings = completing(&asked, "$value");
        meetings.sort_unstable();
        assert_eq!(meetings, (0..200).step_by(25).collect::<Vec<_>>());
        let m13: Vec<&Asked> = asked
            .iter()
            .filter(|a| a.path.starts_with("/me/messages/m13/attachments"))
            .collect();
        assert!(
            m13[1].at - m13[0].at >= Duration::from_secs(1),
            "asked again only after the second Graph asked for"
        );

        let s = lock(&store);
        for id in s.folder_member_ids(folder.id).unwrap() {
            let subject = s.message(id).unwrap().unwrap().subject;
            let remote = subject.strip_prefix("Message ").unwrap();
            let content = s.content(id).unwrap().unwrap();
            let names: Vec<&str> = content
                .attachments
                .iter()
                .map(|a| a.name.as_str())
                .collect();
            match number(remote) {
                // Gone since it was listed, or unreadable: stored without.
                40 | 41 => assert!(names.is_empty(), "{remote}"),
                _ => assert_eq!(names, [format!("{remote}.pdf")], "its own: {remote}"),
            }
            let meeting = number(remote).is_multiple_of(25);
            assert_eq!(content.invite.is_some(), meeting, "{remote}");
        }
        drop(s);
        assert!(
            repair_list(&store, account_id).is_empty(),
            "nothing more will come"
        );
    }

    #[tokio::test]
    async fn a_few_new_messages_come_whole_a_few_at_a_time() {
        let (base, seen) = serve(attached).await;
        let graph = Graph::with_base(&base, "token");
        let (store, account_id) = microsoft_store();
        let folder = projects(&store, account_id);
        let mut state = FolderState {
            limit: Some(FETCH_ONE_BY_ONE),
            ..Default::default()
        };

        // m5 fails: nothing is stored, and the next sync tries again.
        let failed = graph.sync_folder(&store, &folder, &mut state).await;
        assert!(matches!(failed, Err(Error::Server(_))), "{failed:?}");
        let members = lock(&store).folder_member_ids(folder.id).unwrap();
        assert!(members.is_empty());
        assert!(seen.most.load(SeqCst) <= 4);
        seen.take();
        seen.most.store(0, SeqCst);

        let started = std::time::Instant::now();
        let changed = graph.sync_folder(&store, &folder, &mut state).await;
        let elapsed = started.elapsed();
        let asked = seen.take();
        let most = seen.most.load(SeqCst);
        eprintln!(
            "10 new messages one by one: {} requests, at most {most} at once, {elapsed:?}",
            asked.len()
        );
        assert_eq!(changed.unwrap(), Some(9), "m3 went since it was listed");
        let whole: Vec<&Asked> = asked
            .iter()
            .filter(|a| a.path.starts_with("/me/messages/m") && a.path.contains("?$select=subject"))
            .collect();
        assert_eq!(whole.len(), 10);
        assert!(whole.iter().all(|a| {
            a.prefer
                .iter()
                .any(|p| p == "outlook.body-content-type=\"html\"")
        }));
        assert!(immutable(&asked));
        // The listing, ten messages, nine attachment listings and m0's MIME.
        assert_eq!(asked.len(), 1 + 10 + 9 + 1);
        assert!(
            (2..=4).contains(&most),
            "side by side, never more than four: {most}"
        );
    }

    /// Triage of `p0`… as Graph takes it: a flag change, then a move. p7
    /// is gone; p13's move is throttled for a second once (answered at
    /// once), p50's fails once and p90's flag change is cut off once.
    /// Answers about every third message take longer, so they come back
    /// out of order.
    fn triage(asked: &Asked, attempt: usize) -> Answer {
        let rest = asked.path.strip_prefix("/me/messages/").unwrap();
        let (id, part) = rest.split_once('/').unwrap_or((rest, ""));
        let answer = match (asked.method.as_str(), part, number(id)) {
            ("POST", "move", 13) if attempt == 0 => {
                return Answer {
                    retry_after: Some(1),
                    ..Answer::status(429)
                };
            }
            ("PATCH", "", 7) => Answer::status(404),
            ("PATCH", "", 90) if attempt == 0 => Answer::status(0),
            ("POST", "move", 50) if attempt == 0 => Answer::status(500),
            ("PATCH", "", _) | ("POST", "move", _) => Answer::json(json!({ "id": id })),
            _ => Answer::status(400),
        };
        let slow = number(id).is_multiple_of(3);
        answer.after(Duration::from_millis(if slow { 15 } else { 5 }))
    }

    /// `count` messages `p0`… stored unread in the inbox, then read and
    /// marked Done in turn. Returns their ids.
    fn triaged(store: &Mutex<Store>, account_id: i64, count: usize) -> Vec<i64> {
        let mut s = lock(store);
        (0..count)
            .map(|n| {
                let incoming = Incoming {
                    account_id,
                    remote_id: Some(format!("p{n}")),
                    from_address: "ana@contoso.com".into(),
                    unread: true,
                    date: 1,
                    ..Default::default()
                };
                let Added::New(id) = s.add_incoming(&incoming, false).unwrap() else {
                    panic!("p{n} is new");
                };
                let done = spark_store::Patch {
                    state: Some(spark_store::MessageState::Done),
                    unread: Some(false),
                    ..Default::default()
                };
                s.update(id, &done, n as i64 + 1).unwrap();
                id
            })
            .collect()
    }

    /// The remote ids of `ids` that the store has in Archive on the server.
    fn archived(store: &Mutex<Store>, ids: &[i64]) -> HashSet<String> {
        let s = lock(store);
        ids.iter()
            .map(|id| s.location(*id).unwrap().unwrap())
            .filter(|at| at.remote_place == Place::Archive)
            .map(|at| at.remote_id.unwrap())
            .collect()
    }

    #[tokio::test]
    async fn triage_goes_a_few_requests_at_a_time_and_never_twice() {
        let (base, seen) = serve(triage).await;
        let graph = Graph::with_base(&base, "token");
        let (store, account_id) = microsoft_store();
        let ids = triaged(&store, account_id, 200);
        let pending = || lock(&store).pending(account_id, 1_000, 0).unwrap();

        // Pushed until it all goes, each round after a failure.
        let started = std::time::Instant::now();
        let mut errors = Vec::new();
        let mut rounds = Vec::new();
        loop {
            let before = archived(&store, &ids);
            let pushed = graph.push(&store, pending()).await;
            rounds.push((before, seen.take()));
            match pushed {
                Ok(_) => break,
                Err(e) if errors.len() < 5 => errors.push(e),
                Err(e) => panic!("{e}"),
            }
        }
        let elapsed = started.elapsed();
        let asked: Vec<&Asked> = rounds.iter().flat_map(|(_, asked)| asked).collect();
        let most = seen.most.load(SeqCst);
        eprintln!(
            "200 messages read and archived: {} requests over {} rounds, at most {most} at once, {elapsed:?}",
            asked.len(),
            rounds.len()
        );
        assert!(
            matches!(errors[..], [Error::Server(_), Error::Network { .. }]),
            "p50's failure, then p90's: {errors:?}"
        );
        assert_eq!(most, 4, "never more than four at once");
        assert!(rounds.iter().all(|(_, asked)| immutable(asked)));

        // Each message's requests in order: its flags before its move, and
        // nothing again once it's through.
        for n in 0..200 {
            let path = format!("/me/messages/p{n}");
            let methods: Vec<&str> = asked
                .iter()
                .filter(|a| a.path == path || a.path == format!("{path}/move"))
                .map(|a| a.method.as_str())
                .collect();
            let expected: &[&str] = match n {
                7 => &["PATCH"],
                13 => &["PATCH", "POST", "POST"],
                50 => &["PATCH", "POST", "PATCH", "POST"],
                90 => &["PATCH", "PATCH", "POST"],
                _ => &["PATCH", "POST"],
            };
            assert_eq!(methods, expected, "p{n}");
        }
        assert_eq!(asked.len(), 200 + 199 + 4);
        for (before, asked) in &rounds {
            assert!(
                !asked.iter().any(|a| {
                    let id = a.path.strip_prefix("/me/messages/").unwrap();
                    before.contains(id.split('/').next().unwrap())
                }),
                "a round asks nothing of mail pushed before it"
            );
        }
        let p13: Vec<&&Asked> = asked
            .iter()
            .filter(|a| a.path == "/me/messages/p13/move")
            .collect();
        let throttled = p13[0].at;
        assert!(p13[1].at - throttled >= Duration::from_secs(1));
        assert!(
            !asked.iter().any(|a| {
                let after = a.at.saturating_duration_since(throttled);
                after > Duration::from_millis(300) && after < Duration::from_millis(900)
            }),
            "nothing new starts while Graph wants a pause"
        );

        assert!(pending().is_empty());
        let done = archived(&store, &ids);
        assert_eq!(done.len(), 199);
        assert!(!done.contains("p7"), "gone: dropped, not pushed");
    }

    /// Graph throttling `p0`'s move for an hour, answered once the other
    /// moves are under way.
    fn throttling(asked: &Asked, _: usize) -> Answer {
        if asked.path == "/me/messages/p0/move" {
            Answer {
                retry_after: Some(3600),
                ..Answer::status(429)
            }
            .after(Duration::from_millis(100))
        } else {
            Answer::json(json!({})).after(Duration::from_millis(20))
        }
    }

    #[tokio::test]
    async fn a_long_throttle_leaves_the_change_for_a_later_round() {
        let (base, seen) = serve(throttling).await;
        let graph = Graph::with_base(&base, "token");
        let (store, account_id) = microsoft_store();
        let ids = triaged(&store, account_id, 3);
        let pending = lock(&store).pending(account_id, 1_000, 0).unwrap();

        let started = std::time::Instant::now();
        let pushed = graph.push(&store, pending).await;
        assert!(matches!(pushed, Err(Error::Network { .. })), "{pushed:?}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "not waited out"
        );
        let pending = lock(&store).pending(account_id, 1_000, 0).unwrap();
        assert_eq!(
            pending.iter().map(|p| p.id).collect::<Vec<_>>(),
            [ids[0]],
            "p0 stays pending; what went alongside it is through"
        );
        assert_eq!(seen.take().len(), 3 + 3);
    }

    /// The `attached` stand-in, with Graph throttling m0's attachments for
    /// a minute the first time, answered at once, while other attachment
    /// listings take their time.
    fn throttled_for_a_minute(asked: &Asked, attempt: usize) -> Answer {
        if asked.path.starts_with("/me/messages/m0/attachments") && attempt == 0 {
            return Answer {
                retry_after: Some(60),
                ..Answer::status(429)
            };
        }
        let answer = attached(asked, attempt);
        if asked.path.contains("/attachments") {
            answer.after(Duration::from_millis(100))
        } else {
            answer
        }
    }

    /// The account's repair list, as stored.
    fn repair_list(store: &Mutex<Store>, account_id: i64) -> Vec<Repair> {
        let state = stored_state(&lock(store), account_id).unwrap();
        state.unwrap().repair
    }

    /// Lists `repairs` on the account's repair list, as a sync would.
    fn list_for_repair(store: &Mutex<Store>, account_id: i64, repairs: Vec<Repair>) {
        let listed = lock(store).batch(|b| listed_for_repair(b, account_id, repairs));
        listed.unwrap();
    }

    /// The Graph ids on the account's repair list, in its order.
    fn listed(store: &Mutex<Store>, account_id: i64) -> Vec<String> {
        let list = repair_list(store, account_id);
        list.into_iter().map(|r| r.id).collect()
    }

    const FILES: Missing = Missing {
        attachments: true,
        invite: false,
    };

    #[tokio::test]
    async fn a_long_throttle_stores_the_chunk_and_lists_what_it_missed() {
        let (base, seen) = serve(throttled_for_a_minute).await;
        let graph = Graph::with_base(&base, "token");
        let (store, account_id) = microsoft_store();
        let folder = projects(&store, account_id);
        let mut state = FolderState {
            limit: Some(STORE_BATCH),
            ..Default::default()
        };

        // 50 new messages: m0's attachments are throttled while m1's to
        // m3's are under way, and nothing more goes out. All 50 are stored
        // at once, and the others listed for what they're missing.
        let started = std::time::Instant::now();
        let synced = graph.sync_folder(&store, &folder, &mut state).await;
        assert_eq!(synced.unwrap(), Some(50));
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "not waited out"
        );
        let asked = seen.take();
        let mut attachments = completing(&asked, "attachments");
        attachments.sort_unstable();
        assert_eq!(attachments, [0, 1, 2, 3]);
        assert_eq!(asked.len(), 2 + 4, "the listing, its bodies and those four");
        let members = lock(&store).folder_member_ids(folder.id).unwrap();
        assert_eq!(members.len(), 50);
        let missing = repair_list(&store, account_id);
        let expected: Vec<String> = [0]
            .into_iter()
            .chain(4..50)
            .map(|n| format!("m{n}"))
            .collect();
        assert_eq!(
            missing.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            expected
        );
        for r in &missing {
            let meeting = number(&r.id).is_multiple_of(25);
            let wanted = Missing {
                invite: meeting,
                ..FILES
            };
            assert_eq!((r.missing, r.tries), (wanted, 0), "{}", r.id);
        }

        // While the pause lasts, a round's repair sends nothing and keeps
        // them all.
        let now = spark_store::now_ms();
        assert_eq!(graph.repair(&store, account_id, now).await, 0);
        assert!(seen.take().is_empty());
        let mut kept = listed(&store, account_id);
        kept.sort_unstable();
        let mut all = expected;
        all.sort_unstable();
        assert_eq!(kept, all);
        assert!(repair_list(&store, account_id).iter().all(|r| r.tries == 0));

        // Once it's over, rounds fetch them, 20 at a time.
        pause_until(&graph.standing, None);
        let mut rounds = 0;
        let mut changed = 0;
        while !repair_list(&store, account_id).is_empty() {
            rounds += 1;
            assert!(rounds <= 3, "{:?}", listed(&store, account_id));
            changed += graph.repair(&store, account_id, now).await;
            let asked = completing(&seen.take(), "attachments");
            assert!(asked.len() <= REPAIR_BATCH + 1, "round {rounds}: {asked:?}");
        }
        assert_eq!(rounds, 3);
        assert_eq!(changed, 47 - 2, "but m40 and m41");
        let s = lock(&store);
        for id in members {
            let subject = s.message(id).unwrap().unwrap().subject;
            let n = number(subject.strip_prefix("Message ").unwrap());
            let content = s.content(id).unwrap().unwrap();
            // m40's are gone and m41's unreadable, which ends their repair.
            assert_eq!(content.attachments.is_empty(), matches!(n, 40 | 41), "m{n}");
            assert_eq!(content.invite.is_some(), n.is_multiple_of(25), "m{n}");
        }
    }

    /// The `delta` stand-in, with Graph throttling n2's attachments for a
    /// minute the first time.
    fn delta_throttled(asked: &Asked, attempt: usize) -> Answer {
        if asked.path.starts_with("/me/messages/n2/attachments") && attempt == 0 {
            return Answer {
                retry_after: Some(60),
                ..Answer::status(429)
            };
        }
        delta(&asked.path).map_or_else(|| Answer::status(404), Answer::json)
    }

    /// The store's message with `subject`.
    fn titled(store: &Mutex<Store>, subject: &str) -> Option<spark_store::Message> {
        let messages = lock(store).messages().unwrap();
        messages.into_iter().find(|m| m.subject == subject)
    }

    /// The `delta` stand-in's s3, stored unread; returns its id.
    fn unread_s3(store: &Mutex<Store>, account_id: i64) -> i64 {
        let s3 = Incoming {
            account_id,
            remote_id: Some("s3".into()),
            unread: true,
            date: NEWEST,
            ..Default::default()
        };
        let Added::New(id) = lock(store).add_incoming(&s3, false).unwrap() else {
            panic!("s3 is new");
        };
        id
    }

    /// Where a synced account's inbox delta query stands, before `/delta1`.
    fn at_delta1() -> SyncState {
        SyncState {
            delta_link: Some("/delta1".into()),
            reply_to_selected: true,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_long_throttle_stores_the_inbox_page_and_lists_what_it_missed() {
        let (base, seen) = serve(delta_throttled).await;
        let graph = Graph::with_base(&base, "token");
        let (store, account_id) = microsoft_store();
        let s3 = unread_s3(&store, account_id);
        let mut state = at_delta1();

        // Both pages are stored, n2 without its file, and the delta query
        // moves on.
        let report = graph.sync_inbox(&store, account_id, &mut state).await;
        assert_eq!(report.unwrap().new.len(), 2, "n1 and n2");
        assert_eq!(state.delta_link.as_deref(), Some("/delta3"));
        assert!(titled(&store, "Message n2").unwrap().attachment.is_none());
        assert!(!lock(&store).message(s3).unwrap().unwrap().unread);
        assert_eq!(listed(&store, account_id), ["n2"]);
        assert_eq!(completing(&seen.take(), "attachments"), [2]);

        // Its file comes in the first round after the pause.
        let now = spark_store::now_ms();
        assert_eq!(graph.repair(&store, account_id, now).await, 0);
        assert!(seen.take().is_empty());
        pause_until(&graph.standing, None);
        assert_eq!(graph.repair(&store, account_id, now).await, 1);
        let n2 = titled(&store, "Message n2").unwrap();
        assert_eq!(n2.attachment.as_deref(), Some("plan.pdf"));
        assert!(listed(&store, account_id).is_empty());
        assert_eq!(completing(&seen.take(), "attachments"), [2]);
    }

    #[tokio::test]
    async fn files_gone_since_the_listing_are_left_out_and_not_listed() {
        // n2's attachments were deleted since the delta query listed it;
        // listing s3's fails.
        let (base, _) = serve(|asked, _| match asked.path.split('?').next().unwrap() {
            "/me/messages/n2/attachments" => Answer::status(404),
            "/me/messages/s3/attachments" => Answer::status(500),
            _ => delta(&asked.path).map_or_else(|| Answer::status(404), Answer::json),
        })
        .await;
        let graph = Graph::with_base(&base, "token");
        let (store, account_id) = microsoft_store();
        let mut state = at_delta1();

        // Both are stored without them, with the rest of their page, and
        // the delta query moves on. Only s3 is listed: n2's won't come.
        let report = graph.sync_inbox(&store, account_id, &mut state).await;
        assert_eq!(report.unwrap().new.len(), 3, "n1, n2 and s3");
        assert_eq!(state.delta_link.as_deref(), Some("/delta3"));
        for subject in ["Message n2", "Message s3"] {
            let m = titled(&store, subject).unwrap();
            assert!(m.attachment.is_none(), "{subject}");
            let content = lock(&store).content(m.id).unwrap().unwrap();
            assert!(content.attachments.is_empty(), "{subject}");
        }
        let missing = repair_list(&store, account_id);
        let missing: Vec<(&str, Missing)> =
            missing.iter().map(|r| (r.id.as_str(), r.missing)).collect();
        assert_eq!(missing, [("s3", FILES)]);
    }

    #[tokio::test]
    async fn a_message_twice_in_a_delta_page_is_stored_and_listed_once() {
        // n2 comes twice, new and then read. Listing its attachments fails.
        let (base, seen) = serve(|asked, _| {
            let n2 = |read: bool| {
                json!({
                    "id": "n2", "subject": "Message n2", "isRead": read,
                    "receivedDateTime": iso_date(NEWEST), "hasAttachments": true,
                    "from": { "emailAddress": { "name": "Ana", "address": "ana@contoso.com" } },
                    "body": { "contentType": "text", "content": "Hi" },
                })
            };
            match asked.path.split('?').next().unwrap() {
                "/delta1" => Answer::json(json!({
                    "value": [n2(false), n2(true)], "@odata.deltaLink": "/delta2",
                })),
                "/me/messages/n2/attachments" => Answer::status(500),
                _ => Answer::status(404),
            }
        })
        .await;
        let graph = Graph::with_base(&base, "token");
        let (store, account_id) = microsoft_store();
        let mut state = at_delta1();

        let report = graph.sync_inbox(&store, account_id, &mut state).await;
        assert_eq!(report.unwrap().new.len(), 1);
        assert_eq!(state.delta_link.as_deref(), Some("/delta2"));
        assert_eq!(lock(&store).messages().unwrap().len(), 1);
        assert!(
            !titled(&store, "Message n2").unwrap().unread,
            "as it came last"
        );
        assert_eq!(completing(&seen.take(), "attachments"), [2], "asked once");
        let missing = repair_list(&store, account_id);
        assert_eq!(missing.len(), 1);
        assert_eq!((missing[0].id.as_str(), missing[0].missing), ("n2", FILES));
    }

    /// The `delta` stand-in, where Graph throttles n2's attachments three
    /// times over, asking to be tried again at once (four tries each), then
    /// turns them away three times as the sign-in lapsed.
    fn delta_turned_away(asked: &Asked, attempt: usize) -> Answer {
        if asked.path.starts_with("/me/messages/n2/attachments") {
            match attempt {
                0..12 => {
                    return Answer {
                        retry_after: Some(0),
                        ..Answer::status(429)
                    };
                }
                12..15 => return Answer::status(401),
                _ => {}
            }
        }
        delta(&asked.path).map_or_else(|| Answer::status(404), Answer::json)
    }

    #[tokio::test]
    async fn throttling_or_a_lapsed_sign_in_never_gives_up_on_files() {
        let (base, _) = serve(delta_turned_away).await;
        let graph = Graph::with_base(&base, "token");
        let (store, account_id) = microsoft_store();
        let mut state = at_delta1();

        // n2 is stored while it's throttled, without its file.
        graph
            .sync_inbox(&store, account_id, &mut state)
            .await
            .unwrap();
        assert_eq!(state.delta_link.as_deref(), Some("/delta3"));
        assert!(titled(&store, "Message n2").unwrap().attachment.is_none());
        let since = repair_list(&store, account_id)[0].since;
        // As though it had failed often enough of its own already.
        lock(&store)
            .batch(|b| {
                change_repairs(b, account_id, |list| {
                    list[0].tries = REPAIR_TRIES;
                })
            })
            .unwrap();

        // They hold up the account, not n2: they aren't its tries, and
        // however long they last it isn't given up on.
        let later = since + 30 * DAY_MS;
        for round in 1..=5 {
            let changed = graph.repair(&store, account_id, later).await;
            assert_eq!(changed, 0, "round {round}");
            let missing = repair_list(&store, account_id);
            let tries = (missing.len(), missing[0].tries);
            assert_eq!(tries, (1, REPAIR_TRIES), "round {round}");
        }
        assert_eq!(graph.repair(&store, account_id, later).await, 1);
        let n2 = titled(&store, "Message n2").unwrap();
        assert_eq!(n2.attachment.as_deref(), Some("plan.pdf"));
        assert!(repair_list(&store, account_id).is_empty());
    }

    #[tokio::test]
    async fn a_message_graph_keeps_failing_is_given_up_on_after_a_week() {
        // Listing n2's attachments fails every time.
        let (base, seen) = serve(|asked, _| {
            if asked.path.starts_with("/me/messages/n2/attachments") {
                return Answer::status(500);
            }
            delta(&asked.path).map_or_else(|| Answer::status(404), Answer::json)
        })
        .await;
        let graph = Graph::with_base(&base, "token");
        let (store, account_id) = microsoft_store();
        graph
            .sync_inbox(&store, account_id, &mut at_delta1())
            .await
            .unwrap();
        let since = repair_list(&store, account_id)[0].since;
        seen.take();

        // Tried each round, however often, for a week.
        let day = since + DAY_MS;
        for tries in 1..=2 * REPAIR_TRIES {
            assert_eq!(graph.repair(&store, account_id, day).await, 0);
            assert_eq!(seen.take().len(), 1, "one request a round");
            let missing = repair_list(&store, account_id);
            assert_eq!((missing.len(), missing[0].tries), (1, tries));
        }
        let week = since + REPAIR_FOR_MS;
        graph.repair(&store, account_id, week).await;
        assert_eq!(seen.take().len(), 1);
        assert!(repair_list(&store, account_id).is_empty());
        assert!(titled(&store, "Message n2").unwrap().attachment.is_none());
        graph.repair(&store, account_id, week).await;
        assert!(seen.take().is_empty(), "given up on, it's asked no more");

        // A week since it was stored isn't enough without the tries: a
        // computer that was off meanwhile tries again first.
        let n2 = Repair {
            id: "n2".into(),
            missing: FILES,
            since,
            tries: 0,
        };
        list_for_repair(&store, account_id, vec![n2]);
        for tries in 1..REPAIR_TRIES {
            graph.repair(&store, account_id, week).await;
            assert_eq!(repair_list(&store, account_id)[0].tries, tries);
        }
        graph.repair(&store, account_id, week).await;
        assert!(repair_list(&store, account_id).is_empty());
    }

    /// The `attached` stand-in, where i300's MIME fails the first time.
    fn invite_fails_once(asked: &Asked, attempt: usize) -> Answer {
        if asked.path == "/me/messages/i300/$value" && attempt == 0 {
            return Answer::status(500);
        }
        attached(asked, attempt)
    }

    #[tokio::test]
    async fn reaching_back_lists_a_meeting_whose_invite_failed() {
        let (base, _) = serve(invite_fails_once).await;
        let graph = Graph::with_base(&base, "token");
        let (store, account_id) = microsoft_store();
        // The delta window ends between i249 and i250.
        let mut state = SyncState {
            delta_link: Some(format!("{base}/delta")),
            since: Some(received(250) + 1_800_000),
            ..Default::default()
        };

        // The page is stored and reaching back moves on, i300 without its
        // invite, which is listed.
        let stored = graph.older_inbox(&store, account_id, &mut state).await;
        assert_eq!(stored.unwrap(), 100);
        assert_eq!(state.older_than, Some(received(349)));
        let missing = repair_list(&store, account_id);
        let invite = Missing {
            attachments: false,
            invite: true,
        };
        assert_eq!(
            missing
                .iter()
                .map(|r| (r.id.as_str(), r.missing))
                .collect::<Vec<_>>(),
            [("i300", invite)]
        );
        let i300 = titled(&store, "Message i300").unwrap();
        let content = lock(&store).content(i300.id).unwrap().unwrap();
        assert!(content.invite.is_none());
        assert_eq!(content.attachments.len(), 1);

        let now = spark_store::now_ms();
        assert_eq!(graph.repair(&store, account_id, now).await, 1);
        assert!(repair_list(&store, account_id).is_empty());
        let s = lock(&store);
        for m in s.messages().unwrap() {
            let n = number(m.subject.strip_prefix("Message ").unwrap());
            let content = s.content(m.id).unwrap().unwrap();
            assert_eq!(content.invite.is_some(), n.is_multiple_of(25), "i{n}");
            assert_eq!(content.attachments.len(), 1, "i{n}");
        }
    }

    /// The `searchable` stand-in, where a8 is a meeting request whose MIME
    /// is cut off the first time.
    fn searchable_meeting(asked: &Asked, attempt: usize) -> Answer {
        if asked.path == "/me/messages/a8/$value" {
            return match attempt {
                0 => Answer::status(0),
                _ => Answer {
                    body: MEETING.into(),
                    ..Answer::json(Value::Null)
                },
            };
        }
        let Some(mut reply) = searchable(asked) else {
            return Answer::status(404);
        };
        if asked.path.starts_with("/me/messages/a8?") {
            reply["@odata.type"] = json!("#microsoft.graph.eventMessageRequest");
        }
        Answer::json(reply)
    }

    #[tokio::test]
    async fn a_search_lists_the_invites_it_couldnt_fetch() {
        let (base, _) = serve(searchable_meeting).await;
        let graph = Graph::with_base(&base, "token");
        let (store, account_id) = searched_store();
        let state = searchable_state(&base);
        let query = Query::parse("invoice");

        let stored = graph
            .search_server(&store, account_id, &state, &query)
            .await;
        assert_eq!(stored.unwrap(), 3);
        let a8 = titled(&store, "Invoice a8").unwrap();
        assert!(
            lock(&store)
                .content(a8.id)
                .unwrap()
                .unwrap()
                .invite
                .is_none()
        );
        assert_eq!(listed(&store, account_id), ["a8"]);

        let now = spark_store::now_ms();
        assert_eq!(graph.repair(&store, account_id, now).await, 1);
        let content = lock(&store).content(a8.id).unwrap().unwrap();
        assert!(content.invite.is_some());
        assert!(listed(&store, account_id).is_empty());
    }

    /// The `searchable` stand-in, where what a search finds has a file
    /// each, named for it (`a8.pdf`), which Graph fails to list while
    /// `down`.
    fn searchable_with_files(down: Arc<AtomicBool>) -> impl Fn(&Asked, usize) -> Answer {
        move |asked, _| {
            let route = asked.path.split('?').next().unwrap();
            let files = route
                .strip_prefix("/me/messages/")
                .and_then(|rest| rest.strip_suffix("/attachments"));
            if let Some(id) = files {
                if down.load(SeqCst) {
                    return Answer::status(500);
                }
                return Answer::json(json!({ "value": [
                    { "id": "a1", "name": format!("{id}.pdf"), "contentType": "application/pdf", "size": 10 }
                ] }));
            }
            let Some(mut reply) = searchable(asked) else {
                return Answer::status(404);
            };
            if let Some(found) = reply["value"].as_array_mut() {
                for m in found {
                    m["hasAttachments"] = json!(true);
                }
            } else if route.starts_with("/me/messages/") {
                reply["hasAttachments"] = json!(true);
            }
            Answer::json(reply)
        }
    }

    #[tokio::test]
    async fn quick_searches_while_graph_fails_lose_no_files() {
        let down = Arc::new(AtomicBool::new(true));
        let (base, _) = serve(searchable_with_files(down.clone())).await;
        let graph = Graph::with_base(&base, "token");
        let (store, account_id) = searched_store();
        let state = searchable_state(&base);

        // Three searches as the user refines one, while Graph fails to list
        // attachments: what they find is stored, and listed for its files.
        let mut stored = 0;
        for query in ["invoice", "invoice from:ana@contoso.com", "invoice ana"] {
            let query = Query::parse(query);
            let found = graph.search_server(&store, account_id, &state, &query);
            stored += found.await.unwrap();
        }
        assert_eq!(stored, 3);
        let mut missing = listed(&store, account_id);
        missing.sort_unstable();
        assert_eq!(missing, ["a8", "i20", "m5"]);

        // The account's next round once Graph is back fetches every file.
        down.store(false, SeqCst);
        let now = spark_store::now_ms();
        assert_eq!(graph.repair(&store, account_id, now).await, 3);
        assert!(listed(&store, account_id).is_empty());
        for id in ["a8", "i20", "m5"] {
            let m = titled(&store, &format!("Invoice {id}")).unwrap();
            assert_eq!(m.attachment, Some(format!("{id}.pdf")), "{id}");
        }
    }

    #[tokio::test]
    async fn a_chunk_is_stored_with_its_repair_listing_or_not_at_all() {
        // Graph fails to list m7's attachments while `down`.
        let down = Arc::new(AtomicBool::new(true));
        let failing = down.clone();
        let (base, _) = serve(move |asked, attempt| {
            if asked.path.starts_with("/me/messages/m7/attachments") && failing.load(SeqCst) {
                return Answer::status(500);
            }
            attached(asked, attempt)
        })
        .await;
        let graph = Graph::with_base(&base, "token");
        let (store, account_id) = microsoft_store();
        let folder = projects(&store, account_id);
        let mut state = FolderState {
            limit: Some(STORE_BATCH),
            ..Default::default()
        };
        let unsynced = state.clone();

        // The app is killed as it lists m7: none of the chunk is stored.
        LISTING_FAILS.set(true);
        let killed = graph.sync_folder(&store, &folder, &mut state).await;
        assert!(killed.is_err());
        assert!(lock(&store).messages().unwrap().is_empty());
        assert!(listed(&store, account_id).is_empty());
        assert_eq!(state, unsynced);

        // Started again, the folder's next sync stores it all, and lists
        // m7, whose file comes once Graph gives it.
        let synced = graph.sync_folder(&store, &folder, &mut state).await;
        assert_eq!(synced.unwrap(), Some(50));
        assert_eq!(listed(&store, account_id), ["m7"]);
        down.store(false, SeqCst);
        let now = spark_store::now_ms();
        assert_eq!(graph.repair(&store, account_id, now).await, 1);
        let m7 = titled(&store, "Message m7").unwrap();
        assert_eq!(m7.attachment.as_deref(), Some("m7.pdf"));
    }

    #[tokio::test]
    async fn reaching_back_stores_a_page_with_its_repair_listing_or_not_at_all() {
        let (base, _) = serve(invite_fails_once).await;
        let graph = Graph::with_base(&base, "token");
        let (store, account_id) = microsoft_store();
        let mut state = SyncState {
            delta_link: Some(format!("{base}/delta")),
            since: Some(received(250) + 1_800_000),
            ..Default::default()
        };
        let unsynced = state.clone();

        // Killed as it lists i300's invite: none of the page is stored, and
        // reaching back hasn't moved.
        LISTING_FAILS.set(true);
        let killed = graph.older_inbox(&store, account_id, &mut state).await;
        assert!(killed.is_err());
        assert!(lock(&store).messages().unwrap().is_empty());
        assert!(listed(&store, account_id).is_empty());
        assert_eq!(state, unsynced);

        // The next try gets the page again, i300 with its invite this time.
        let stored = graph.older_inbox(&store, account_id, &mut state).await;
        assert_eq!(stored.unwrap(), 100);
        let i300 = titled(&store, "Message i300").unwrap();
        let content = lock(&store).content(i300.id).unwrap().unwrap();
        assert!(content.invite.is_some());
        assert!(listed(&store, account_id).is_empty());
    }

    #[tokio::test]
    async fn a_search_stores_what_it_finds_with_its_repair_listing_or_not_at_all() {
        let down = Arc::new(AtomicBool::new(true));
        let (base, _) = serve(searchable_with_files(down.clone())).await;
        let graph = Graph::with_base(&base, "token");
        let (store, account_id) = searched_store();
        let state = searchable_state(&base);
        let query = Query::parse("invoice");

        // Killed as it lists what it found: none of it is stored.
        LISTING_FAILS.set(true);
        let killed = graph.search_server(&store, account_id, &state, &query);
        assert!(killed.await.is_err());
        assert_eq!(
            lock(&store).messages().unwrap().len(),
            1,
            "k4, stored before"
        );
        assert!(listed(&store, account_id).is_empty());

        // Searched again, it's stored and listed, and the files come.
        let found = graph.search_server(&store, account_id, &state, &query);
        assert_eq!(found.await.unwrap(), 3);
        let mut missing = listed(&store, account_id);
        missing.sort_unstable();
        assert_eq!(missing, ["a8", "i20", "m5"]);
        down.store(false, SeqCst);
        let now = spark_store::now_ms();
        assert_eq!(graph.repair(&store, account_id, now).await, 3);
    }

    /// Stores m`n` for each of `numbers` without its file, and lists it
    /// for repair, in that order.
    fn stored_without_files(store: &Mutex<Store>, account_id: i64, numbers: Range<usize>) {
        let mut repairs = Vec::new();
        for n in numbers {
            let m = Incoming {
                account_id,
                remote_id: Some(format!("m{n}")),
                subject: format!("Message m{n}"),
                date: received(n),
                ..Default::default()
            };
            lock(store).add_incoming(&m, false).unwrap();
            repairs.push(Repair {
                id: format!("m{n}"),
                missing: FILES,
                since: 0,
                tries: 0,
            });
        }
        list_for_repair(store, account_id, repairs);
    }

    /// A stand-in for Graph listing a message's file, named for it
    /// (`m7.pdf`): at once, but m0's to m3's take far longer than a round.
    fn slow_m0_to_m3(asked: &Asked, _: usize) -> Answer {
        let rest = asked.path.strip_prefix("/me/messages/");
        let Some((id, _)) = rest.and_then(|rest| rest.split_once("/attachments")) else {
            return Answer::status(404);
        };
        let answer = Answer::json(json!({ "value": [
            { "id": "a1", "name": format!("{id}.pdf"), "contentType": "application/pdf", "size": 10 }
        ] }));
        match number(id) {
            0..4 => answer.after(Duration::from_secs(600)),
            _ => answer,
        }
    }

    /// The ids and tries on the account's repair list, sorted.
    fn tries(store: &Mutex<Store>, account_id: i64) -> Vec<(String, u32)> {
        let list = repair_list(store, account_id);
        let mut tries: Vec<_> = list.into_iter().map(|r| (r.id, r.tries)).collect();
        tries.sort_unstable();
        tries
    }

    /// The files stored for m`n` for each of `numbers`.
    fn files_of(store: &Mutex<Store>, numbers: Range<usize>) -> Vec<Option<String>> {
        numbers
            .map(|n| titled(store, &format!("Message m{n}")).unwrap().attachment)
            .collect()
    }

    #[tokio::test]
    async fn a_repair_too_slow_fails_a_try_of_its_own_and_lets_the_rest_through() {
        let (base, seen) = serve(slow_m0_to_m3).await;
        let graph = Graph {
            repair_request_time: Duration::from_millis(300),
            ..Graph::with_base(&base, "token")
        };
        let (store, account_id) = microsoft_store();
        stored_without_files(&store, account_id, 0..8);

        // The round waits out the account's pause, longer than a request
        // may take, which doesn't count against them. Then m0 to m3 take
        // too long: each fails a try of its own and goes to the end of the
        // list, and m4 to m7 come after them.
        let paused = tokio::time::Instant::now() + Duration::from_millis(600);
        pause_until(&graph.standing, Some(paused));
        let now = spark_store::now_ms();
        assert_eq!(graph.repair(&store, account_id, now).await, 4);
        assert!(seen.take().iter().all(|a| a.at >= paused.into_std()));
        let failed: Vec<_> = (0..4).map(|n| (format!("m{n}"), 1)).collect();
        assert_eq!(tries(&store, account_id), failed);
        let came: Vec<_> = (4..8).map(|n| Some(format!("m{n}.pdf"))).collect();
        assert_eq!(files_of(&store, 4..8), came);
        assert!(files_of(&store, 0..4).iter().all(Option::is_none));

        // Each round counts another.
        assert_eq!(graph.repair(&store, account_id, now).await, 0);
        let failed: Vec<_> = (0..4).map(|n| (format!("m{n}"), 2)).collect();
        assert_eq!(tries(&store, account_id), failed);
    }

    #[tokio::test]
    async fn what_a_repair_round_has_no_time_left_for_goes_to_the_end() {
        let (base, _) = serve(slow_m0_to_m3).await;
        // The round runs out before a request would.
        let graph = Graph {
            repair_time: Duration::from_millis(500),
            ..Graph::with_base(&base, "token")
        };
        let (store, account_id) = microsoft_store();
        stored_without_files(&store, account_id, 0..8);

        // m0 to m3 are cut off and go to the end of the list, not counting
        // it as a try: they may have been waiting out the account's pause.
        // m4 to m7 never started, and keep their places.
        let now = spark_store::now_ms();
        assert_eq!(graph.repair(&store, account_id, now).await, 0);
        let list = listed(&store, account_id);
        assert_eq!(list[..4], ["m4", "m5", "m6", "m7"]);
        let mut cut = list[4..].to_vec();
        cut.sort_unstable();
        assert_eq!(cut, ["m0", "m1", "m2", "m3"]);
        assert!(repair_list(&store, account_id).iter().all(|r| r.tries == 0));

        // So the next round comes to m4 to m7.
        assert_eq!(graph.repair(&store, account_id, now).await, 4);
        let came: Vec<_> = (4..8).map(|n| Some(format!("m{n}.pdf"))).collect();
        assert_eq!(files_of(&store, 4..8), came);
        let cut: Vec<_> = (0..4).map(|n| (format!("m{n}"), 0)).collect();
        assert_eq!(tries(&store, account_id), cut);
    }

    #[test]
    fn repair_lists_survive_saving_the_rest_of_the_state() {
        let (store, account_id) = microsoft_store();
        let n2 = Repair {
            id: "n2".into(),
            missing: FILES,
            since: 5,
            tries: 0,
        };
        list_for_repair(&store, account_id, vec![n2.clone()]);
        // A round's copy, read before n2 was listed, moves the delta link.
        let round = SyncState {
            delta_link: Some("/delta2".into()),
            ..Default::default()
        };
        save_state(&lock(&store), account_id, &round).unwrap();
        let stored = stored_state(&lock(&store), account_id).unwrap().unwrap();
        assert_eq!(stored.delta_link.as_deref(), Some("/delta2"));
        assert_eq!(stored.repair, std::slice::from_ref(&n2));

        // Listed again for its invite: once, missing both.
        let invite = Repair {
            missing: Missing {
                attachments: false,
                invite: true,
            },
            since: 9,
            ..n2.clone()
        };
        list_for_repair(&store, account_id, vec![invite]);
        let both = Repair {
            missing: Missing {
                attachments: true,
                invite: true,
            },
            ..n2
        };
        assert_eq!(repair_list(&store, account_id), [both]);
        let json = lock(&store)
            .account(account_id)
            .unwrap()
            .unwrap()
            .sync_state;
        assert!(
            json.contains(
                r#""repair":[{"id":"n2","attachments":true,"invite":true,"since":5,"tries":0}]"#
            ),
            "{json}"
        );
    }
}
