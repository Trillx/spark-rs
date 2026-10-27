//! IMAP: connecting, finding the archive and trash folders, syncing the
//! inbox into the store, listing and syncing other folders, and pushing
//! local changes back.
//!
//! The first inbox sync takes the newest messages (up to a limit) and
//! remembers the lowest UID taken as the floor; later syncs look at
//! everything from the floor up, so new mail and mail moved back into the
//! inbox both show up, while older mail stays on the server until the user
//! scrolls to it: reaching back fetches the next messages below the floor
//! and lowers it. Other folders are synced only when the user wants them,
//! and only their newest messages, a window that reaching back widens.
//!
//! Of mail already stored only flags are asked for again (Message-ID and
//! Reply-To never change), and with CONDSTORE only the flags that changed
//! since the last sync.
//!
//! FETCH and UID SEARCH go through `fetch`, `uid_fetch` and `uid_search`
//! here, not async-imap's: a server's NO or BAD fails them, where
//! async-imap's take it for an answer with nothing (or only part) in it.
//! Where part of an answer is still good (the bodies, headers and flags of
//! the messages that came), a NO is read as `Completion::Partial` instead,
//! and a sync goes on without what it held back: a message the server
//! can't fetch (Gmail: "Some messages could not be FETCHed") holds back
//! only itself, and is asked for again by later syncs.

use std::collections::HashSet;
use std::io;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::task::{Context, Poll};
use std::time::Duration;

use async_imap::imap_proto::{
    AttributeValue, MailboxDatum, MessageSection, NameAttribute, Outcome, Response, SectionPath,
    Status,
};
use async_imap::types::{Fetch, Flag};
use async_imap::{Client, Session as ImapSession};
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use spark_store::{
    Account, AccountKind, Added, Folder, FolderRole, Incoming, Location, Pending, Place, Pushed,
    Query, RemoteFolder, Store,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::task::{JoinHandle, spawn_blocking};
use tokio::time::timeout;
use tokio_rustls::client::TlsStream;

use crate::search::{self, SERVER_HITS};
use crate::{BLOCKED, Error, OLDER_STEP, Result, Service, parse, tls, utf7};

pub const INBOX: &str = "INBOX";
/// Messages fetched on the first sync of an account.
pub const FIRST_SYNC_LIMIT: usize = 300;
/// A folder shows its newest this many messages.
pub const FOLDER_LIMIT: u32 = 200;
/// Bigger messages are stored without their body.
const MAX_BODY_BYTES: u32 = 10 * 1024 * 1024;
const FETCH_BATCH: usize = 25;
/// Most flag and header observations stored in one transaction.
const OBSERVE_BATCH: usize = 500;
/// Most mail one fetch of bodies asks for, by the server's sizes.
const FETCH_BYTES: u64 = 16 * 1024 * 1024;
/// How many UIDs below the floor reaching back first searches; each search
/// after that is twice as wide.
const OLDER_SPAN: u32 = 2 * OLDER_STEP as u32;
/// Message-ID and Reply-To, the headers stored mail is checked against.
const REPLY_TO_HEADERS: &str = "BODY.PEEK[HEADER.FIELDS (MESSAGE-ID REPLY-TO)]";
const NETWORK_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Security {
    /// TLS from the first byte (ports 993, 465).
    Tls,
    /// Plain connection upgraded with STARTTLS (ports 143, 587).
    StartTls,
    /// No encryption. Only for local test servers.
    Plain,
}

impl Security {
    pub fn for_port(port: u16) -> Self {
        match port {
            993 | 465 => Self::Tls,
            _ => Self::StartTls,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Server {
    pub host: String,
    pub port: u16,
    pub security: Security,
    /// The fingerprint (`tls::fingerprint`) of a certificate the system
    /// doesn't trust and the user does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trusted_cert: Option<String>,
}

impl Server {
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.trim().to_owned(),
            port,
            security: Security::for_port(port),
            trusted_cert: None,
        }
    }
}

/// Where an account's sync stands, kept as JSON in the store.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncState {
    pub uid_validity: Option<u32>,
    /// Lowest inbox UID that is synced.
    pub floor: Option<u32>,
    /// The inbox has nothing below the floor: no older mail to reach back for.
    #[serde(default)]
    pub complete: bool,
    pub archive: Option<String>,
    pub trash: Option<String>,
    /// Where mail from blocked senders goes; found or made the first time
    /// some has to.
    #[serde(default)]
    pub blocked: Option<String>,
    /// The Reply-To of the inbox mail stored has been fetched. Caches from
    /// before Reply-To was kept fetch it once; mail fetched since has it.
    #[serde(default)]
    pub reply_to_fetched: bool,
    /// Until then, the UIDs whose Reply-To is still to fetch: at first all
    /// the inbox mail stored, then what the server didn't give yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reply_to_pending: Vec<u32>,
    /// On servers with CONDSTORE, the inbox's HIGHESTMODSEQ when the flags
    /// of all the mail tracked were last brought in: the next sync asks
    /// only for flags changed since.
    #[serde(default)]
    pub modseq: Option<u64>,
}

/// Where a folder's sync stands, kept as JSON with the folder.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderState {
    pub uid_validity: Option<u32>,
    /// How many of its newest messages the folder shows; `FOLDER_LIMIT`
    /// until the user reaches further back.
    #[serde(default)]
    pub limit: Option<u32>,
    /// The folder has no messages beyond the limit.
    #[serde(default)]
    pub complete: bool,
    /// The Reply-To of the mail it lists has been fetched (as for the inbox).
    #[serde(default)]
    pub reply_to_fetched: bool,
    /// Until then, the UIDs whose Reply-To is still to fetch (as for the inbox).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reply_to_pending: Vec<u32>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct FolderReport {
    /// Messages that came into or left the folder's list.
    pub changed: usize,
    /// The server no longer has the folder.
    pub missing: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Ids of the messages stored for the first time.
    pub new: Vec<i64>,
    /// Messages that left the inbox elsewhere.
    pub gone: usize,
    /// Cached messages whose Reply-To headers changed.
    pub headers: usize,
    pub first: bool,
}

#[derive(Debug)]
pub enum Stream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_flush(cx),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

pub type Session = ImapSession<Stream>;

/// Connects and logs in. A wrong password comes back as `Error::Auth`.
pub async fn login(server: &Server, username: &str, password: &str) -> Result<Session> {
    let tcp = timeout(
        NETWORK_TIMEOUT,
        TcpStream::connect((server.host.as_str(), server.port)),
    )
    .await
    .map_err(|_| Error::network(&server.host, "timed out"))?
    .map_err(|e| Error::network(&server.host, e))?;
    let client = match server.security {
        Security::Plain => greeted(Client::new(Stream::Plain(tcp))).await?,
        Security::Tls => {
            greeted(Client::new(Stream::Tls(Box::new(tls(server, tcp).await?)))).await?
        }
        Security::StartTls => {
            let mut client = greeted(Client::new(Stream::Plain(tcp))).await?;
            client.run_command_and_check_ok("STARTTLS", None).await?;
            let Stream::Plain(tcp) = client.into_inner() else {
                unreachable!("STARTTLS starts from a plain stream")
            };
            // No second greeting after STARTTLS.
            Client::new(Stream::Tls(Box::new(tls(server, tcp).await?)))
        }
    };
    let session = timeout(NETWORK_TIMEOUT, client.login(username, password))
        .await
        .map_err(|_| Error::network(&server.host, "timed out"))?;
    session.map_err(|(e, _)| match e {
        async_imap::error::Error::No(text) | async_imap::error::Error::Bad(text) => {
            Error::Auth(text)
        }
        other => other.into(),
    })
}

async fn greeted(mut client: Client<Stream>) -> Result<Client<Stream>> {
    match timeout(NETWORK_TIMEOUT, client.read_response()).await {
        Ok(Ok(Some(_))) => Ok(client),
        Ok(Ok(None)) => Err(Error::Server("The server closed the connection.".into())),
        Ok(Err(e)) => Err(e.into()),
        Err(_) => Err(Error::Server("The server didn't greet us.".into())),
    }
}

async fn tls(server: &Server, tcp: TcpStream) -> Result<TlsStream<TcpStream>> {
    let host = &server.host;
    let trusted = server.trusted_cert.as_deref();
    timeout(
        NETWORK_TIMEOUT,
        tls::connect(Service::Imap, host, tcp, trusted),
    )
    .await
    .map_err(|_| Error::network(host, "TLS handshake timed out"))?
}

/// Finds (or, if the server has none, creates) the folders Done and Delete
/// move mail to: special-use `\Archive`, else `\All` (Gmail's All Mail),
/// else one named "Archive"; `\Trash`, else one named like a trash folder.
pub async fn find_folders(session: &mut Session, state: &mut SyncState) -> Result<()> {
    if state.archive.is_some() && state.trash.is_some() {
        return Ok(());
    }
    let names: Vec<_> = session
        .list(Some(""), Some("*"))
        .await?
        .try_collect()
        .await?;
    let with = |attr: NameAttribute<'static>| {
        names
            .iter()
            .find(|n| n.attributes().contains(&attr))
            .map(|n| n.name().to_owned())
    };
    let named = |candidates: &[&str]| {
        names
            .iter()
            .find(|n| candidates.iter().any(|c| n.name().eq_ignore_ascii_case(c)))
            .map(|n| n.name().to_owned())
    };
    let archive = with(NameAttribute::Archive)
        .or_else(|| with(NameAttribute::All))
        .or_else(|| named(&["Archive", "Archives"]));
    let trash = with(NameAttribute::Trash)
        .or_else(|| named(&["Trash", "Deleted Items", "Deleted Messages", "Deleted"]));
    state.archive = Some(match archive {
        Some(name) => name,
        None => {
            session.create("Archive").await?;
            "Archive".into()
        }
    });
    state.trash = Some(match trash {
        Some(name) => name,
        None => {
            session.create("Trash").await?;
            "Trash".into()
        }
    });
    Ok(())
}

/// The account's folders. INBOX isn't one (the Inbox is always there), and
/// folders nested in it are listed at the top. Roles come from special-use
/// attributes, else common names; `state` names the archive and trash Done
/// and Delete use. Gmail's `[Gmail]` container is left out, its folders
/// listed at the top.
pub async fn list_folders(session: &mut Session, state: &SyncState) -> Result<Vec<RemoteFolder>> {
    let names: Vec<_> = session
        .list(Some(""), Some("*"))
        .await?
        .try_collect()
        .await?;
    let listed: Vec<Listed> = names
        .iter()
        .map(|n| Listed {
            name: n.name().to_owned(),
            delimiter: n.delimiter().map(str::to_owned),
            attributes: n.attributes().iter().filter_map(attribute).collect(),
        })
        .collect();
    Ok(folders_from(&listed, state))
}

/// A LIST reply, reduced to what `folders_from` needs.
#[derive(Clone, Debug)]
struct Listed {
    name: String,
    delimiter: Option<String>,
    attributes: Vec<Attr>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Attr {
    NoSelect,
    NonExistent,
    All,
    Archive,
    Drafts,
    Flagged,
    Junk,
    Sent,
    Trash,
}

fn attribute(a: &NameAttribute<'_>) -> Option<Attr> {
    Some(match a {
        NameAttribute::NoSelect => Attr::NoSelect,
        NameAttribute::All => Attr::All,
        NameAttribute::Archive => Attr::Archive,
        NameAttribute::Drafts => Attr::Drafts,
        NameAttribute::Flagged => Attr::Flagged,
        NameAttribute::Junk => Attr::Junk,
        NameAttribute::Sent => Attr::Sent,
        NameAttribute::Trash => Attr::Trash,
        NameAttribute::Extension(e) if e.eq_ignore_ascii_case("\\NonExistent") => Attr::NonExistent,
        _ => return None,
    })
}

fn folders_from(listed: &[Listed], state: &SyncState) -> Vec<RemoteFolder> {
    let has = |attr: Attr| listed.iter().any(|l| l.attributes.contains(&attr));
    let leaf = |l: &Listed| -> String {
        let name = l.name.as_str();
        l.delimiter
            .as_deref()
            .and_then(|d| name.rsplit_once(d))
            .map_or(name, |(_, leaf)| leaf)
            .to_owned()
    };
    // Servers without special-use attributes: go by the usual names.
    let named = |l: &Listed, names: &[&str]| names.iter().any(|n| leaf(l).eq_ignore_ascii_case(n));
    let role = |l: &Listed| -> Option<Option<FolderRole>> {
        let a = &l.attributes;
        if state.archive.as_deref() == Some(l.name.as_str()) {
            return Some(Some(FolderRole::Archive));
        }
        if state.trash.as_deref() == Some(l.name.as_str()) {
            return Some(Some(FolderRole::Trash));
        }
        if state.blocked.as_deref() == Some(l.name.as_str())
            || (state.blocked.is_none() && is_blocked_folder(l))
        {
            return Some(Some(FolderRole::Blocked));
        }
        // Another archive, trash or all-mail folder would show mail twice.
        if a.contains(&Attr::All) || a.contains(&Attr::Archive) || a.contains(&Attr::Trash) {
            return None;
        }
        Some(
            if a.contains(&Attr::Sent)
                || (!has(Attr::Sent)
                    && named(l, &["Sent", "Sent Items", "Sent Messages", "Sent Mail"]))
            {
                Some(FolderRole::Sent)
            } else if a.contains(&Attr::Drafts)
                || (!has(Attr::Drafts) && named(l, &["Drafts", "Draft"]))
            {
                Some(FolderRole::Drafts)
            } else if a.contains(&Attr::Junk)
                || (!has(Attr::Junk)
                    && named(
                        l,
                        &["Junk", "Spam", "Junk E-mail", "Junk Email", "Bulk Mail"],
                    ))
            {
                Some(FolderRole::Spam)
            } else if a.contains(&Attr::Flagged) {
                Some(FolderRole::Flagged)
            } else {
                None
            },
        )
    };
    // `[Gmail]`-style containers: not shown; their folders move up.
    let container = |name: &str| {
        listed.iter().any(|l| {
            l.name == name
                && l.attributes.contains(&Attr::NoSelect)
                && l.name.starts_with('[')
                && l.name.ends_with(']')
        })
    };
    let mut folders = Vec::new();
    for l in listed {
        if l.name.eq_ignore_ascii_case(INBOX)
            || l.attributes.contains(&Attr::NonExistent)
            || container(&l.name)
        {
            continue;
        }
        let Some(role) = role(l) else { continue };
        let parent = l
            .delimiter
            .as_deref()
            .and_then(|d| l.name.rsplit_once(d))
            .map(|(parent, _)| parent)
            .filter(|p| !p.eq_ignore_ascii_case(INBOX) && !container(p))
            .map(str::to_owned);
        folders.push(RemoteFolder {
            remote_id: l.name.clone(),
            parent,
            name: utf7::decode(&leaf(l)),
            role,
            selectable: !l.attributes.contains(&Attr::NoSelect),
        });
    }
    folders
}

/// Spark's Blocked folder: "Blocked" at the top, or in INBOX on servers
/// that keep every folder there.
fn is_blocked_folder(l: &Listed) -> bool {
    let name = l.name.as_str();
    let nested = l
        .delimiter
        .as_deref()
        .and_then(|d| name.split_once(d))
        .filter(|(parent, _)| parent.eq_ignore_ascii_case(INBOX))
        .map(|(_, leaf)| leaf);
    nested.unwrap_or(name).eq_ignore_ascii_case(BLOCKED)
}

/// The account's Blocked folder: the one found before, else one already
/// on the server, else a new one.
async fn blocked_folder(session: &mut Session, state: &mut SyncState) -> Result<String> {
    if let Some(name) = &state.blocked {
        return Ok(name.clone());
    }
    let names: Vec<_> = session
        .list(Some(""), Some("*"))
        .await?
        .try_collect()
        .await?;
    let found = names.iter().find(|n| {
        is_blocked_folder(&Listed {
            name: n.name().to_owned(),
            delimiter: n.delimiter().map(str::to_owned),
            attributes: Vec::new(),
        })
    });
    let name = match found {
        Some(n) => n.name().to_owned(),
        None => create_folder(session, None, BLOCKED).await?,
    };
    state.blocked = Some(name.clone());
    Ok(name)
}

/// Brings a folder's newest messages (up to its limit) into the store and
/// makes them its list. Mail already stored (found by Message-ID) isn't
/// downloaded again, so raising the limit fetches only the older ones.
pub async fn sync_folder(
    session: &mut Session,
    store: &Mutex<Store>,
    folder: &Folder,
    state: &mut FolderState,
) -> Result<FolderReport> {
    let mailbox = match session.examine(&folder.remote_id).await {
        Ok(mailbox) => mailbox,
        Err(async_imap::error::Error::No(_)) => {
            return Ok(FolderReport {
                missing: true,
                ..Default::default()
            });
        }
        Err(e) => return Err(e.into()),
    };
    if mailbox.uid_validity != state.uid_validity {
        if state.uid_validity.is_some() {
            lock(store).forget_folder_uids(folder.id)?;
        }
        state.uid_validity = mailbox.uid_validity;
        state.reply_to_pending.clear();
    }

    // The newest messages by sequence number. Of the mail it lists
    // already, the flags are stored as they come. A list the server cut
    // short (NO) may leave out mail still there, so then nothing leaves
    // the folder, or the Reply-To still to fetch.
    let limit = state.limit.unwrap_or(FOLDER_LIMIT);
    state.complete = mailbox.exists <= limit;
    let known = lock(store).folder_uids(folder.id)?;
    let mut window: Vec<u32> = Vec::new();
    let mut whole = true;
    if mailbox.exists > 0 {
        let first = mailbox.exists.saturating_sub(limit) + 1;
        let fetched = fetch(session, format!("{first}:*"), "(UID FLAGS)").await?;
        let completion = flags_in_chunks(fetched, |chunk| {
            window.extend(chunk.iter().map(|(uid, ..)| *uid));
            store_flags(store, folder.account_id, &folder.remote_id, chunk, |uid| {
                known.contains_key(&uid)
            })
        })
        .await?;
        if let Completion::Partial(said) = completion {
            skipped(&folder.remote_id, "part of the list", &said);
            whole = false;
        }
    }

    let listed: Vec<u32> = window
        .iter()
        .copied()
        .filter(|uid| known.contains_key(uid))
        .collect();
    let mut report = FolderReport::default();
    if !state.reply_to_fetched {
        // At first all the mail stored in it. What it lists is asked for;
        // after a whole list, what it doesn't list has left it.
        let pending = &mut state.reply_to_pending;
        if pending.is_empty() {
            pending.extend(known.keys());
            pending.sort_unstable();
        }
        let listing: HashSet<u32> = listed.iter().copied().collect();
        pending.retain(|uid| known.contains_key(uid) && (!whole || listing.contains(uid)));
        report.changed += fetch_reply_to(
            session,
            store,
            folder.account_id,
            &folder.remote_id,
            pending,
            |uid| listing.contains(&uid),
        )
        .await?;
        state.reply_to_fetched = pending.is_empty();
    }
    let mut members: Vec<(i64, Option<u32>)> =
        listed.iter().map(|uid| (known[uid], Some(*uid))).collect();

    let new: Vec<u32> = window
        .iter()
        .copied()
        .filter(|uid| !known.contains_key(uid))
        .collect();
    for batch in new.chunks(FETCH_BATCH * 4) {
        let (heads, _) = uid_fetch_each(
            session,
            &folder.remote_id,
            batch,
            &format!("(UID FLAGS {REPLY_TO_HEADERS})"),
        )
        .await?;
        let heads: Vec<Incoming> = heads
            .iter()
            .filter_map(|f| {
                let (unread, flagged) = read_flags(f);
                Some(Incoming {
                    account_id: folder.account_id,
                    mailbox: Some(folder.remote_id.clone()),
                    uid: Some(f.uid?),
                    message_id: f.header().and_then(parse::message_id),
                    reply_to: f.header().and_then(parse::reply_to),
                    unread,
                    flagged,
                    ..Default::default()
                })
            })
            .collect();
        // Mail stored already (from the inbox, another label) joins the
        // folder as it is; the rest is downloaded.
        let mut elsewhere = Vec::new();
        let mut unknown = Vec::new();
        {
            let store = lock(store);
            for m in heads {
                let stored = match &m.message_id {
                    Some(id) => store.has_message_id(folder.account_id, id)?,
                    None => false,
                };
                if stored {
                    elsewhere.push(m);
                } else {
                    unknown.extend(m.uid);
                }
            }
        }
        members.extend(add_to_folder(store, folder.id, &elsewhere)?);
        report.changed += elsewhere.len();
        let mut fetched = fetch_messages(session, &unknown, &folder.remote_id).await?;
        for m in &mut fetched {
            m.account_id = folder.account_id;
        }
        members.extend(add_to_folder(store, folder.id, &fetched)?);
        report.changed += fetched.len();
    }
    if whole {
        report.changed += lock(store).set_folder_members(folder.id, &members)?;
    }
    Ok(report)
}

/// Stores mail a folder sync found, all in one transaction, and returns
/// each one's stored id and UID for the folder's members.
fn add_to_folder(
    store: &Mutex<Store>,
    folder_id: i64,
    messages: &[Incoming],
) -> Result<Vec<(i64, Option<u32>)>> {
    if messages.is_empty() {
        return Ok(Vec::new());
    }
    Ok(lock(store).batch(|b| {
        messages
            .iter()
            .map(|m| Ok((added_id(b.add_folder_message(folder_id, m)?), m.uid)))
            .collect()
    })?)
}

fn added_id(added: Added) -> i64 {
    match added {
        Added::New(id) | Added::Known(id) => id,
    }
}

/// Creates a folder, in `parent` (a mailbox name) or at the top, and
/// returns its mailbox name.
pub async fn create_folder(
    session: &mut Session,
    parent: Option<&str>,
    name: &str,
) -> Result<String> {
    let encoded = utf7::encode(name);
    let mailbox = match parent {
        Some(parent) => {
            let delimiter = delimiter(session, parent)
                .await?
                .unwrap_or_else(|| "/".into());
            format!("{parent}{delimiter}{encoded}")
        }
        None => encoded.clone(),
    };
    match session.create(&mailbox).await {
        Ok(()) => Ok(mailbox),
        // Some servers keep every folder inside INBOX.
        Err(async_imap::error::Error::No(_)) if parent.is_none() => {
            let delimiter = delimiter(session, INBOX)
                .await?
                .unwrap_or_else(|| ".".into());
            let nested = format!("{INBOX}{delimiter}{encoded}");
            session.create(&nested).await?;
            Ok(nested)
        }
        Err(e) => Err(e.into()),
    }
}

/// Renames a folder in place (same parent) and returns its new mailbox name.
pub async fn rename_folder(session: &mut Session, mailbox: &str, name: &str) -> Result<String> {
    let delimiter = delimiter(session, mailbox).await?;
    let renamed = match delimiter.as_deref().and_then(|d| mailbox.rsplit_once(d)) {
        Some((parent, _)) => format!(
            "{parent}{}{}",
            delimiter.unwrap_or_default(),
            utf7::encode(name)
        ),
        None => utf7::encode(name),
    };
    session.rename(mailbox, &renamed).await?;
    Ok(renamed)
}

/// Deletes folders, given innermost first.
pub async fn delete_folders(session: &mut Session, mailboxes: &[String]) -> Result<()> {
    for mailbox in mailboxes {
        session.delete(mailbox).await?;
    }
    Ok(())
}

/// Whether the selected mailbox still has `uid`.
async fn still_there(session: &mut Session, uid: u32) -> Result<bool> {
    let fetched: Vec<Fetched> = uid_fetch(session, uid.to_string(), "(UID)")
        .await?
        .try_collect()
        .await?;
    Ok(fetched.iter().any(|f| f.uid == Some(uid)))
}

/// Whether the server has `mailbox`.
async fn exists(session: &mut Session, mailbox: &str) -> Result<bool> {
    let names: Vec<_> = session
        .list(Some(""), Some(mailbox))
        .await?
        .try_collect()
        .await?;
    Ok(names.iter().any(|n| n.name() == mailbox))
}

/// The hierarchy delimiter the server uses at `mailbox`.
async fn delimiter(session: &mut Session, mailbox: &str) -> Result<Option<String>> {
    let names: Vec<_> = session
        .list(Some(""), Some(mailbox))
        .await?
        .try_collect()
        .await?;
    Ok(names.iter().find_map(|n| n.delimiter().map(str::to_owned)))
}

/// Of `senders`, the ones the user has written to: the Sent mailbox has
/// mail to them. SEARCH matches parts of addresses (ann@ in joann@), so
/// what it finds is checked for the exact address (the newest
/// `SENT_CANDIDATES`). Addresses that can't be searched for are left out.
pub async fn written_to(
    session: &mut Session,
    sent: &str,
    senders: &[String],
) -> Result<Vec<String>> {
    session.examine(sent).await?;
    let mut found = Vec::new();
    for address in senders
        .iter()
        .filter(|a| a.is_ascii() && !a.contains(['"', '\\']))
    {
        let q = quoted(address);
        let mut uids: Vec<u32> = uid_search(session, format!("OR OR TO {q} CC {q} BCC {q}"))
            .await?
            .into_iter()
            .collect();
        if uids.is_empty() {
            continue;
        }
        uids.sort_unstable();
        let newest = &uids[uids.len().saturating_sub(SENT_CANDIDATES)..];
        let headers: Vec<Fetched> = uid_fetch(
            session,
            uid_set(newest),
            "(UID BODY.PEEK[HEADER.FIELDS (TO CC BCC)])",
        )
        .await?
        .try_collect()
        .await?;
        let wanted = address.to_lowercase();
        if headers.iter().any(|f| {
            f.header()
                .is_some_and(|h| parse::recipient_addresses(h).contains(&wanted))
        }) {
            found.push(address.clone());
        }
    }
    Ok(found)
}

/// How many of the Sent mail a search found are checked for the exact
/// address, at most: headers only, in one fetch.
const SENT_CANDIDATES: usize = 500;

/// Brings the inbox into the store: new messages, flag changes, and
/// messages that left the inbox elsewhere.
pub async fn sync_inbox(
    session: &mut Session,
    store: &Mutex<Store>,
    account_id: i64,
    state: &mut SyncState,
) -> Result<SyncReport> {
    // SELECT turns CONDSTORE on, and until then a server may keep
    // HIGHESTMODSEQ back. A modseq kept means the server has it; without
    // one, CAPABILITY says. A server that turns it away is selected
    // plainly, and asked for every flag.
    let mut condstore =
        state.modseq.is_some() || session.capabilities().await?.has_str("CONDSTORE");
    let mut selected = None;
    if condstore {
        match session.select_condstore(INBOX).await {
            Ok(mailbox) => selected = Some(mailbox),
            Err(async_imap::error::Error::Bad(_)) => condstore = false,
            Err(e) => return Err(e.into()),
        }
    }
    let mailbox = match selected {
        Some(mailbox) => mailbox,
        None => session.select(INBOX).await?,
    };
    if !condstore {
        state.modseq = None;
    }
    if mailbox.uid_validity != state.uid_validity {
        // The server renumbered the inbox. Stored messages are found again
        // by Message-ID as they're fetched.
        if state.uid_validity.is_some() {
            lock(store).forget_uids(account_id, INBOX)?;
        }
        state.uid_validity = mailbox.uid_validity;
        state.floor = None;
        state.modseq = None;
        state.reply_to_pending.clear();
    }

    let first = state.floor.is_none();
    let (floor, present, complete) = match state.floor {
        Some(floor) => {
            // Mail only ever comes in above the floor (UIDs only grow), so
            // once nothing is below it, nothing ever is.
            let complete = state.complete
                || lowest_uid(session, mailbox.exists)
                    .await?
                    .is_none_or(|lowest| lowest >= floor);
            // "floor:*" also matches the newest message if that's below it.
            let present: HashSet<u32> = uid_search(session, format!("UID {floor}:*"))
                .await?
                .into_iter()
                .filter(|uid| *uid >= floor)
                .collect();
            (floor, present, complete)
        }
        // The newest messages, by sequence number.
        None => {
            let limit = FIRST_SYNC_LIMIT as u32;
            let mut present = HashSet::new();
            if mailbox.exists > 0 {
                let first = mailbox.exists.saturating_sub(limit) + 1;
                let fetched: Vec<Fetched> = fetch(session, format!("{first}:*"), "(UID)")
                    .await?
                    .try_collect()
                    .await?;
                present.extend(
                    fetched
                        .iter()
                        .filter(|f| f.message >= first)
                        .filter_map(|f| f.uid),
                );
            }
            let floor = present
                .iter()
                .min()
                .copied()
                .unwrap_or_else(|| mailbox.uid_next.unwrap_or(1));
            (floor, present, mailbox.exists <= limit)
        }
    };
    let known = lock(store).uids(account_id, INBOX)?;

    let mut new: Vec<u32> = present.difference(&known).copied().collect();
    new.sort_unstable();
    let mut report = SyncReport {
        first,
        ..Default::default()
    };
    for batch in new.chunks(FETCH_BATCH) {
        report
            .new
            .extend(fetch_new(session, store, account_id, batch, !first).await?);
    }

    if !state.reply_to_fetched {
        // At first all the mail stored. Mail no longer stored left the inbox.
        let pending = &mut state.reply_to_pending;
        if pending.is_empty() {
            pending.extend(&known);
            pending.sort_unstable();
        }
        pending.retain(|uid| known.contains(uid));
        report.headers +=
            fetch_reply_to(session, store, account_id, INBOX, pending, |_| true).await?;
        state.reply_to_fetched = pending.is_empty();
    }

    // Only from a SELECT that turned CONDSTORE on: a server that sends
    // HIGHESTMODSEQ without CHANGEDSINCE would fail every sync.
    let mut modseq = mailbox.highest_modseq.filter(|_| condstore);
    // Flags of everything else we track in the inbox: with CONDSTORE, of
    // what changed since the last sync.
    let tracked = |uid: u32| uid >= floor && known.contains(&uid);
    if known.iter().any(|uid| tracked(*uid)) {
        let since = match (state.modseq, mailbox.highest_modseq) {
            // Lower than before (a server that rebuilt its index): all of them.
            (Some(since), Some(now)) if now >= since => Some(since),
            _ => None,
        };
        let query = match since {
            Some(since) => format!("(UID FLAGS) (CHANGEDSINCE {since})"),
            None => "(UID FLAGS)".into(),
        };
        let fetched = uid_fetch(session, format!("{floor}:*"), query).await?;
        let completion = flags_in_chunks(fetched, |chunk| {
            store_flags(store, account_id, INBOX, chunk, tracked)
        })
        .await?;
        // The flags that came are good. The next sync asks for the rest
        // again: from the same modseq, or all of them.
        if let Completion::Partial(said) = completion {
            skipped(INBOX, "some flags", &said);
            modseq = since;
        }
    }
    state.modseq = modseq;

    // Mail a search brought in from below the floor isn't tracked until
    // reaching back gets to it.
    let gone: Vec<u32> = known
        .iter()
        .copied()
        .filter(|uid| *uid >= floor && !present.contains(uid))
        .collect();
    if !gone.is_empty() {
        report.gone = lock(store).observe_gone_uids(account_id, INBOX, &gone)?;
    }
    state.floor = Some(floor);
    state.complete = complete;
    Ok(report)
}

/// Reaches further back in the inbox: fetches the newest `OLDER_STEP`
/// messages below the floor that aren't stored yet (a search may have
/// stored some), and lowers the floor to the oldest of them, so later
/// syncs track them like the rest. Stored as on a first sync: no
/// screening. Returns how many were new. Does nothing before the first
/// sync, or if the server renumbered the inbox (the next sync starts over).
pub async fn older_inbox(
    session: &mut Session,
    store: &Mutex<Store>,
    account_id: i64,
    state: &mut SyncState,
) -> Result<usize> {
    let Some(floor) = state.floor else {
        return Ok(0);
    };
    let mailbox = session.select(INBOX).await?;
    if mailbox.uid_validity != state.uid_validity {
        return Ok(0);
    }
    let lowest = lowest_uid(session, mailbox.exists).await?;
    let known = lock(store).uids(account_id, INBOX)?;
    // Down to the `OLDER_STEP`th one not stored yet, and on past any stored
    // below it: mail a search stored is passed, not fetched again. The UIDs
    // below the floor are searched a range at a time, each range twice as
    // wide as the last, until one more not stored turns up (`more`) or the
    // lowest is reached.
    let mut older = Vec::new();
    let mut unknown = 0;
    let mut more = false;
    let mut top = floor;
    let mut span = OLDER_SPAN;
    while let Some(lowest) = lowest.filter(|lowest| !more && *lowest < top) {
        let bottom = top.saturating_sub(span).max(lowest);
        let mut page: Vec<u32> = uid_search(session, format!("UID {bottom}:{}", top - 1))
            .await?
            .into_iter()
            .filter(|uid| (bottom..top).contains(uid))
            .collect();
        page.sort_unstable_by(|a, b| b.cmp(a));
        for uid in page {
            if !known.contains(&uid) {
                if unknown == OLDER_STEP {
                    more = true;
                    break;
                }
                unknown += 1;
            }
            older.push(uid);
        }
        top = bottom;
        span = span.saturating_mul(2);
    }

    let mut stored = 0;
    for batch in older.chunks(FETCH_BATCH) {
        let new: Vec<u32> = batch
            .iter()
            .copied()
            .filter(|uid| !known.contains(uid))
            .collect();
        stored += fetch_new(session, store, account_id, &new, false)
            .await?
            .len();
        // Mail a search stored brings its flags from then: the next sync
        // asks for every flag, not just the changes.
        if new.len() < batch.len() {
            state.modseq = None;
        }
        // Lowered batch by batch (they're newest first), so mail stored
        // before a failure is above the floor, where syncs track it.
        state.floor = batch.last().copied();
    }
    state.complete = !more;
    Ok(stored)
}

/// The lowest UID in the selected mailbox, `exists` messages long: its
/// first message's. `None` if it's empty.
async fn lowest_uid(session: &mut Session, exists: u32) -> Result<Option<u32>> {
    if exists == 0 {
        return Ok(None);
    }
    let first: Vec<Fetched> = fetch(session, "1", "(UID)").await?.try_collect().await?;
    Ok(first.iter().filter_map(|f| f.uid).min())
}

/// Reads a fetch's UIDs and flags (unread, flagged) `OBSERVE_BATCH`
/// messages at a time, handing each chunk to `each` as it arrives, so the
/// responses held stay bounded however many come back. If `each` fails,
/// the rest is read (to leave the session ready for its next command)
/// and the failure returned. Returns how the fetch ended: after a NO, the
/// flags that came were handed on, and are good.
async fn flags_in_chunks(
    fetched: impl futures_util::Stream<Item = async_imap::error::Result<Fetched>>,
    mut each: impl FnMut(&[(u32, bool, bool)]) -> Result<()>,
) -> Result<Completion> {
    let mut fetched = std::pin::pin!(fetched);
    let mut chunk = Vec::with_capacity(OBSERVE_BATCH);
    let mut handed = Ok(());
    let completion = loop {
        let fetch = match next_record(&mut fetched).await? {
            ControlFlow::Continue(fetch) => fetch,
            ControlFlow::Break(completion) => break completion,
        };
        let Some(uid) = fetch.uid.filter(|_| handed.is_ok()) else {
            continue;
        };
        let (unread, flagged) = read_flags(&fetch);
        chunk.push((uid, unread, flagged));
        if chunk.len() == OBSERVE_BATCH {
            handed = each(&chunk);
            chunk.clear();
        }
    };
    handed?;
    if !chunk.is_empty() {
        each(&chunk)?;
    }
    Ok(completion)
}

/// Stores the server's flags for the UIDs in `mailbox` that `wanted` keeps,
/// in one transaction.
fn store_flags(
    store: &Mutex<Store>,
    account_id: i64,
    mailbox: &str,
    flags: &[(u32, bool, bool)],
    wanted: impl Fn(u32) -> bool,
) -> Result<()> {
    Ok(lock(store).batch(|b| {
        for &(uid, unread, flagged) in flags.iter().filter(|(uid, ..)| wanted(*uid)) {
            b.observe_flags(account_id, mailbox, uid, unread, flagged)?;
        }
        Ok(())
    })?)
}

/// Fetches the Message-ID and Reply-To of the stored mail at the UIDs in
/// `pending` that `wanted` keeps, in the selected `mailbox`,
/// `OBSERVE_BATCH` at a time, and brings the stored Reply-To up to date.
/// Caches from before Reply-To was kept do this until `pending` is empty.
/// As each batch is stored, its UIDs leave `pending`, but for those the
/// server wouldn't give, even alone, or gave without the headers: they're
/// asked for again next round. (Those an OK left out are gone.) Returns
/// how many changed.
async fn fetch_reply_to(
    session: &mut Session,
    store: &Mutex<Store>,
    account_id: i64,
    mailbox: &str,
    pending: &mut Vec<u32>,
    wanted: impl Fn(u32) -> bool,
) -> Result<usize> {
    let asked: Vec<u32> = pending.iter().copied().filter(|uid| wanted(*uid)).collect();
    let mut changed = 0;
    for chunk in asked.chunks(OBSERVE_BATCH) {
        let (fetched, refused) = uid_fetch_each(
            session,
            mailbox,
            chunk,
            &format!("(UID {REPLY_TO_HEADERS})"),
        )
        .await?;
        let headers: Vec<Incoming> = fetched
            .iter()
            .filter_map(|f| {
                let fields = f.header()?;
                Some(Incoming {
                    account_id,
                    mailbox: Some(mailbox.into()),
                    uid: Some(f.uid?),
                    message_id: parse::message_id(fields),
                    reply_to: parse::reply_to(fields),
                    ..Default::default()
                })
            })
            .collect();
        changed += lock(store).batch(|b| {
            let mut changed = 0;
            for m in &headers {
                changed += usize::from(b.observe_reply_to(m)?);
            }
            Ok(changed)
        })?;
        let came: HashSet<u32> = headers.iter().filter_map(|m| m.uid).collect();
        let held: HashSet<u32> = fetched
            .iter()
            .filter_map(|f| f.uid)
            .chain(refused)
            .filter(|uid| !came.contains(uid))
            .collect();
        let done: HashSet<u32> = chunk
            .iter()
            .copied()
            .filter(|uid| !held.contains(uid))
            .collect();
        pending.retain(|uid| !done.contains(uid));
    }
    Ok(changed)
}

/// Puts a search to the server: the inbox below the floor (mail above it is
/// the inbox sync's) and the archive folder (Gmail: All Mail, in Gmail's
/// own syntax). The newest `SERVER_HITS` of each that aren't stored yet are
/// stored, inbox mail as reaching back stores it and archived mail as the
/// archive folder's sync does. Returns how many were new. A place that
/// renumbered its UIDs is left to its next sync.
pub async fn search_server(
    session: &mut Session,
    store: &Mutex<Store>,
    account: &Account,
    state: &SyncState,
    query: &Query,
) -> Result<usize> {
    let literal = search::has_non_ascii(query) && {
        let server = session.capabilities().await?;
        server.has_str("LITERAL+") || server.has_str("LITERAL-")
    };
    let keys = match account.kind {
        AccountKind::Gmail => search::gmail_keys(query, literal),
        _ => search::imap_keys(query, literal),
    };
    let Some(keys) = keys else {
        return Ok(0);
    };
    let mut stored = 0;

    if let Some(floor) = state.floor
        && session.examine(INBOX).await?.uid_validity == state.uid_validity
    {
        let known = lock(store).uids(account.id, INBOX)?;
        let hits = newest(uid_search(session, &keys).await?, |uid| {
            uid < floor && !known.contains(&uid)
        });
        for batch in hits.chunks(FETCH_BATCH) {
            stored += fetch_new(session, store, account.id, batch, false)
                .await?
                .len();
        }
    }

    let archive = match &state.archive {
        Some(mailbox) => lock(store)
            .folders()?
            .into_iter()
            .find(|f| f.account_id == account.id && f.remote_id == *mailbox),
        None => None,
    };
    let Some(folder) = archive else {
        return Ok(stored);
    };
    let synced: FolderState = serde_json::from_str(&folder.sync_state).unwrap_or_default();
    match session.examine(&folder.remote_id).await {
        Ok(mailbox)
            if synced.uid_validity.is_none() || mailbox.uid_validity == synced.uid_validity => {}
        Ok(_) | Err(async_imap::error::Error::No(_)) => return Ok(stored),
        Err(e) => return Err(e.into()),
    }
    let known = lock(store).folder_uids(folder.id)?;
    let hits = newest(uid_search(session, &keys).await?, |uid| {
        !known.contains_key(&uid)
    });
    for batch in hits.chunks(FETCH_BATCH) {
        // Mail stored from elsewhere (the inbox, another label) is left as it is.
        let (heads, _) = uid_fetch_each(
            session,
            &folder.remote_id,
            batch,
            "(UID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])",
        )
        .await?;
        let heads: Vec<(u32, Option<String>)> = heads
            .iter()
            .filter_map(|f| Some((f.uid?, f.header().and_then(parse::message_id))))
            .collect();
        let mut unknown = Vec::new();
        {
            let store = lock(store);
            for (uid, message_id) in heads {
                let stored_already = match message_id {
                    Some(id) => store.has_message_id(account.id, &id)?,
                    None => false,
                };
                if !stored_already {
                    unknown.push(uid);
                }
            }
        }
        let mut fetched = fetch_messages(session, &unknown, &folder.remote_id).await?;
        for m in &mut fetched {
            m.account_id = account.id;
        }
        stored += lock(store).batch(|b| {
            let mut new = 0;
            for m in &fetched {
                if let Added::New(_) = b.add_folder_message(folder.id, m)? {
                    new += 1;
                }
            }
            Ok(new)
        })?;
    }
    Ok(stored)
}

/// The newest (highest) `SERVER_HITS` of a search's UIDs that are `wanted`.
fn newest(found: HashSet<u32>, wanted: impl Fn(u32) -> bool) -> Vec<u32> {
    let mut uids: Vec<u32> = found.into_iter().filter(|uid| wanted(*uid)).collect();
    uids.sort_unstable_by(|a, b| b.cmp(a));
    uids.truncate(SERVER_HITS);
    uids
}

/// Fetches inbox messages and stores them in one transaction; returns the
/// ids of those that were new.
async fn fetch_new(
    session: &mut Session,
    store: &Mutex<Store>,
    account_id: i64,
    uids: &[u32],
    screen_new_senders: bool,
) -> Result<Vec<i64>> {
    let mut fetched = fetch_messages(session, uids, INBOX).await?;
    for m in &mut fetched {
        m.account_id = account_id;
    }
    // All of them are stored, or (if one fails) none.
    Ok(lock(store).batch(|b| {
        let mut added = Vec::new();
        for m in &fetched {
            if let Added::New(id) = b.add_incoming(m, screen_new_senders)? {
                added.push(id);
            }
        }
        Ok(added)
    })?)
}

/// Fetches and parses messages from the selected `mailbox`, with their
/// location, flags and arrival time; big ones without their body. The
/// account is left for the caller. Bodies come `FETCH_BYTES` at a time and
/// are parsed as they arrive, so the raw mail held stays bounded however
/// many messages are asked for. Those a NO held back are asked for one at
/// a time, and any the server won't give even alone are left out: not
/// stored, they're asked for again by the next sync that wants them.
async fn fetch_messages(
    session: &mut Session,
    uids: &[u32],
    mailbox: &str,
) -> Result<Vec<Incoming>> {
    if uids.is_empty() {
        return Ok(Vec::new());
    }
    let (sizes, _) = uid_fetch_each(session, mailbox, uids, "(UID RFC822.SIZE)").await?;
    let (small, large): (Vec<_>, Vec<_>) = sizes
        .iter()
        .filter_map(|f| Some((f.uid?, f.size.unwrap_or(0))))
        .partition(|(_, size)| *size <= MAX_BODY_BYTES);

    let mut fetches: Vec<(Vec<u32>, &str)> = batches(&small)
        .into_iter()
        .map(|uids| (uids, "(UID FLAGS INTERNALDATE BODY.PEEK[])"))
        .collect();
    if !large.is_empty() {
        let uids = large.into_iter().map(|(uid, _)| uid).collect();
        fetches.push((uids, "(UID FLAGS INTERNALDATE BODY.PEEK[HEADER])"));
    }
    let mut messages = Vec::new();
    for (uids, query) in fetches {
        let mut came = HashSet::new();
        let fetched = uid_fetch(session, uid_set(&uids), query)
            .await?
            .inspect_ok(|f| came.extend(f.uid));
        let completion = parse_each(fetched, incoming, &mut messages).await?;
        for uid in held_back(mailbox, &uids, &came, completion).0 {
            let fetched = uid_fetch(session, uid.to_string(), query).await?;
            if let Completion::Partial(said) = parse_each(fetched, incoming, &mut messages).await? {
                skipped(mailbox, &format!("message {uid}"), &said);
            }
        }
    }
    for m in &mut messages {
        m.mailbox = Some(mailbox.into());
    }
    Ok(messages)
}

/// Splits messages (UID and size, in order) into fetches of at most
/// `FETCH_BATCH` messages and `FETCH_BYTES` of mail; a bigger message is
/// fetched alone.
fn batches(sized: &[(u32, u32)]) -> Vec<Vec<u32>> {
    let mut batches: Vec<Vec<u32>> = Vec::new();
    let mut bytes = 0;
    for &(uid, size) in sized {
        let size = u64::from(size);
        match batches.last_mut() {
            Some(batch) if batch.len() < FETCH_BATCH && bytes + size <= FETCH_BYTES => {
                batch.push(uid);
                bytes += size;
            }
            _ => {
                batches.push(vec![uid]);
                bytes = size;
            }
        }
    }
    batches
}

/// Parses each fetched message on a blocking thread as it arrives, one at a
/// time, reading the next meanwhile: at most two raw messages are held, the
/// one being parsed and the one being read (for a moment twice, as
/// `Fetched` copies it out of its response). Messages that can't be parsed
/// are left out. Returns how the fetch ended: after a NO, the messages that
/// came are good. A failed read (or a BAD) fails the lot.
async fn parse_each<T: Send + 'static>(
    fetched: impl futures_util::Stream<Item = async_imap::error::Result<T>>,
    parse: fn(T) -> Option<Incoming>,
    messages: &mut Vec<Incoming>,
) -> Result<Completion> {
    let mut fetched = std::pin::pin!(fetched);
    let mut parsing = None;
    let completion = loop {
        let fetch = match next_record(&mut fetched).await? {
            ControlFlow::Continue(fetch) => fetch,
            ControlFlow::Break(completion) => break completion,
        };
        if let Some(previous) = parsing.take() {
            messages.extend(parsed(previous).await?);
        }
        parsing = Some(spawn_blocking(move || parse(fetch)));
    };
    if let Some(last) = parsing {
        messages.extend(parsed(last).await?);
    }
    Ok(completion)
}

/// A parse's result; a panic in it carries on here as it would have inline.
async fn parsed(parse: JoinHandle<Option<Incoming>>) -> Result<Option<Incoming>> {
    match parse.await {
        Ok(m) => Ok(m),
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(_) => Err(Error::Server("Reading a message was stopped.".into())),
    }
}

/// A fetched message with its UID, flags and arrival time; `None` if it
/// can't be parsed, or came without the body or header asked for (stored
/// empty, it would never be asked for again). A big one, fetched without
/// its body, says so instead.
fn incoming(fetch: Fetched) -> Option<Incoming> {
    let uid = fetch.uid?;
    let raw = fetch.body().or_else(|| fetch.header())?;
    let mut m = parse::parse_message(raw)?;
    if fetch.body().is_none() {
        m.text = "This message is too large to show here.".into();
        m.snippet = m.text.clone();
    }
    let (unread, flagged) = read_flags(&fetch);
    m.uid = Some(uid);
    m.unread = unread;
    m.flagged = flagged;
    if let Some(date) = fetch.arrived {
        m.date = date;
    }
    Some(m)
}

/// Sends local changes to the server: flags (read, pinned as flagged), then
/// the move for Done, Delete, blocking, filing into a folder, or back to the
/// inbox. Returns how many messages were updated. A message the server no
/// longer has is skipped for good; a move the server refuses waits for the
/// next round. Records the Blocked folder in `state`
/// when it's first needed, and forgets it if it's gone (renamed or deleted
/// elsewhere), leaving those moves to the next round.
pub async fn push(
    session: &mut Session,
    store: &Mutex<Store>,
    state: &mut SyncState,
    pending: Vec<Pending>,
) -> Result<usize> {
    if pending.is_empty() {
        return Ok(0);
    }
    if pending.iter().any(|p| p.place == Place::Blocked) {
        blocked_folder(session, state).await?;
    }
    let caps = session.capabilities().await?;
    let server = Capabilities {
        can_move: caps.has_str("MOVE"),
        uidplus: caps.has_str("UIDPLUS"),
        gmail: caps.has_str("X-GM-EXT-1"),
    };
    let mut pushed = 0;
    for p in pending {
        // The Blocked folder went missing below: these wait for the next
        // round to find or make it again.
        if p.place == Place::Blocked && state.blocked.is_none() {
            continue;
        }
        let Some((mailbox, uid)) = find(session, store, state, &p).await? else {
            lock(store).drop_pending(p.id)?;
            continue;
        };

        // Check before changing flags or copying: without a scoped expunge,
        // a move could permanently delete other clients' deleted messages.
        let destination = folder(state, p.place)
            .or_else(|| p.folder.as_ref().map(|(_, home)| home.clone()))
            .unwrap_or_else(|| mailbox.clone());
        let how = transfer(server, state, &mailbox);
        if destination != mailbox && how == Transfer::Unsupported {
            return Err(Error::Server(
                "This mail server needs MOVE or UIDPLUS to move mail safely.".into(),
            ));
        }

        let mut flags = Vec::new();
        if p.unread != p.remote_unread {
            flags.push((
                if p.unread {
                    "-FLAGS.SILENT"
                } else {
                    "+FLAGS.SILENT"
                },
                "\\Seen",
            ));
        }
        if p.pinned != p.remote_pinned {
            flags.push((
                if p.pinned {
                    "+FLAGS.SILENT"
                } else {
                    "-FLAGS.SILENT"
                },
                "\\Flagged",
            ));
        }
        for (op, flag) in flags {
            let result: async_imap::error::Result<Vec<Fetch>> = async {
                session
                    .uid_store(uid.to_string(), format!("{op} ({flag})"))
                    .await?
                    .try_collect()
                    .await
            }
            .await;
            match result {
                Ok(_) => {}
                Err(async_imap::error::Error::No(_)) => break,
                Err(e) => return Err(e.into()),
            }
        }

        // Filed mail goes to its home folder; without one it stays put.
        let moved = if destination != mailbox {
            let done = match how {
                Transfer::Move => session.uid_mv(uid.to_string(), &destination).await,
                Transfer::CopyAndExpunge => move_by_copy(session, uid, &destination).await,
                Transfer::Copy => session.uid_copy(uid.to_string(), &destination).await,
                Transfer::Unsupported => {
                    unreachable!("unsupported moves were rejected before flags")
                }
            };
            match done {
                Ok(()) => true,
                // Renamed or deleted elsewhere: it's found or made again
                // next round, and the move waits for it.
                Err(async_imap::error::Error::No(_))
                    if p.place == Place::Blocked && !exists(session, &destination).await? =>
                {
                    state.blocked = None;
                    continue;
                }
                // Refused while the message is still there (over quota, say):
                // the move waits for the next round. Gone meanwhile: it's dropped.
                Err(async_imap::error::Error::No(_)) => {
                    if !still_there(session, uid).await? {
                        lock(store).drop_pending(p.id)?;
                    }
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        } else {
            false
        };
        // After a move the new UID isn't known: the next sync of that
        // mailbox, or the next push, finds the message there by Message-ID.
        // Knowing the mailbox keeps other Gmail labels' folders from
        // taking it over.
        let (at_mailbox, at_uid) = if moved {
            (destination.as_str(), None)
        } else {
            (mailbox.as_str(), Some(uid))
        };
        lock(store).mark_pushed(
            &p,
            Pushed {
                mailbox: Some(at_mailbox),
                uid: at_uid,
            },
        )?;
        pushed += 1;
    }
    Ok(pushed)
}

/// Where a message is on the server now, with that mailbox selected: at its
/// known location if that folder still exists; else found by Message-ID
/// (mail that left the inbox), or for filed mail, in a folder that lists it.
/// `None` if the server no longer has it.
async fn find(
    session: &mut Session,
    store: &Mutex<Store>,
    state: &SyncState,
    p: &Pending,
) -> Result<Option<(String, u32)>> {
    if let (Some(mailbox), Some(uid)) = (&p.mailbox, p.uid) {
        match session.select(mailbox).await {
            // A UID identifies a message only within its UIDVALIDITY
            // generation. The next sync may not have noticed a reset yet.
            Ok(current) => {
                if uid_is_current(store, state, p, mailbox, current.uid_validity)? {
                    return Ok(Some((mailbox.clone(), uid)));
                }
            }
            // Renamed or deleted elsewhere.
            Err(async_imap::error::Error::No(_)) => {}
            Err(e) => return Err(e.into()),
        }
    }
    locate(session, store, state, p).await
}

/// Deletes stored messages from the server for good, wherever it has them
/// now, and from the store. On Gmail, expunging mail from a label only
/// takes the label off, so mail outside Trash and Spam goes to Trash first
/// and is expunged there. Needs UIDPLUS: a plain EXPUNGE would also delete
/// what other clients marked deleted. Returns how many went.
pub async fn delete_forever(
    session: &mut Session,
    store: &Mutex<Store>,
    state: &SyncState,
    spam: Option<&str>,
    ids: &[i64],
) -> Result<usize> {
    let caps = session.capabilities().await?;
    if !caps.has_str("UIDPLUS") {
        return Err(Error::Server(
            "This mail server can't delete single emails for good (it lacks UIDPLUS).".into(),
        ));
    }
    let gmail = caps.has_str("X-GM-EXT-1");
    let trash = folder(state, Place::Trash).expect("Trash has a mailbox");
    let mut removed = 0;
    for &id in ids {
        let Some(p) = lock(store).pending_for(id)? else {
            continue;
        };
        let mut found = find(session, store, state, &p).await?;
        if gmail
            && let Some((mailbox, uid)) = &found
            && *mailbox != trash
            && Some(mailbox.as_str()) != spam
        {
            session.uid_mv(uid.to_string(), &trash).await?;
            found = search(session, &trash, &p).await?;
        }
        if let Some((_, uid)) = found {
            let _: Vec<Fetch> = session
                .uid_store(uid.to_string(), "+FLAGS.SILENT (\\Deleted)")
                .await?
                .try_collect()
                .await?;
            let _: Vec<_> = session
                .uid_expunge(uid.to_string())
                .await?
                .try_collect()
                .await?;
        }
        // Gone from the server either way.
        removed += lock(store).remove_messages(&[id])?;
    }
    Ok(removed)
}

/// Deletes every message in a mailbox for good. Returns how many there were.
pub async fn empty_folder(session: &mut Session, mailbox: &str) -> Result<u32> {
    let selected = session.select(mailbox).await?;
    if selected.exists == 0 {
        return Ok(0);
    }
    let _: Vec<Fetch> = session
        .uid_store("1:*", "+FLAGS.SILENT (\\Deleted)")
        .await?
        .try_collect()
        .await?;
    // Everything in the mailbox goes, so a plain EXPUNGE is safe here.
    let _: Vec<_> = session.expunge().await?.try_collect().await?;
    Ok(selected.exists)
}

/// The whole raw message, wherever the server keeps it now: at its known
/// mailbox and UID, else found by Message-ID in the folder it was last
/// pushed to, then the inbox. `None` if it's gone.
pub async fn fetch_raw(
    session: &mut Session,
    state: &SyncState,
    at: &Location,
) -> Result<Option<Vec<u8>>> {
    Ok(fetch_raw_checked(session, state, at, None)
        .await?
        .map(|(raw, _)| raw))
}

/// `fetch_raw`, given the UIDVALIDITY the stored UID was given under, if
/// it's known. When the mailbox has another now, the UID may name some
/// other message, so it isn't fetched: the message is found by Message-ID
/// instead. Also says whether what was found is safe to keep: not what was
/// found that way, as another message may have the same Message-ID.
pub async fn fetch_raw_checked(
    session: &mut Session,
    state: &SyncState,
    at: &Location,
    validity: Option<u32>,
) -> Result<Option<(Vec<u8>, bool)>> {
    let mut renumbered = false;
    if let (Some(mailbox), Some(uid)) = (&at.mailbox, at.uid) {
        let selected = session.select(mailbox).await?;
        renumbered = validity.is_some_and(|v| selected.uid_validity != Some(v));
        if !renumbered && let Some(raw) = fetch_body(session, uid).await? {
            return Ok(Some((raw, true)));
        }
    }
    let Some(message_id) = &at.message_id else {
        return Ok(None);
    };
    let mut folders: Vec<String> = Vec::new();
    for mailbox in at
        .mailbox
        .clone()
        .into_iter()
        .chain(folder(state, at.remote_place))
        .chain([INBOX.to_owned()])
    {
        if !folders.contains(&mailbox) {
            folders.push(mailbox);
        }
    }
    for mailbox in folders {
        match session.select(&mailbox).await {
            Ok(_) => {}
            // The folder doesn't exist (yet).
            Err(async_imap::error::Error::No(_)) => continue,
            Err(e) => return Err(e.into()),
        }
        let query = format!("HEADER Message-ID {}", quoted(message_id));
        let found = uid_search(session, query).await?;
        if let Some(uid) = found.into_iter().max()
            && let Some(raw) = fetch_body(session, uid).await?
        {
            return Ok(Some((raw, !renumbered)));
        }
    }
    Ok(None)
}

async fn fetch_body(session: &mut Session, uid: u32) -> Result<Option<Vec<u8>>> {
    let fetched: Vec<Fetched> = uid_fetch(session, uid.to_string(), "(UID BODY.PEEK[])")
        .await?
        .try_collect()
        .await?;
    Ok(fetched.into_iter().find_map(|f| f.body))
}

/// Where a message with no usable location is now, with that mailbox
/// selected: by Message-ID in the mailbox it was last moved to; else filed
/// mail in a folder that lists it, other mail by Message-ID in the folder of
/// its place.
async fn locate(
    session: &mut Session,
    store: &Mutex<Store>,
    state: &SyncState,
    p: &Pending,
) -> Result<Option<(String, u32)>> {
    if let Some(mailbox) = &p.mailbox
        && let Some(found) = search(session, mailbox, p).await?
    {
        return Ok(Some(found));
    }
    let Some(mailbox) = folder(state, p.remote_place) else {
        let candidates = lock(store).member_locations(p.id)?;
        for (mailbox, uid) in candidates {
            if p.message_id.is_some() {
                if let Some(found) = search(session, &mailbox, p).await? {
                    return Ok(Some(found));
                }
            } else {
                match session.select(&mailbox).await {
                    Ok(current) => {
                        if uid_is_current(store, state, p, &mailbox, current.uid_validity)? {
                            return Ok(Some((mailbox, uid)));
                        }
                    }
                    Err(async_imap::error::Error::No(_)) => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        return out_of_blocked(session, state, p).await;
    };
    match search(session, &mailbox, p).await? {
        Some(found) => Ok(Some(found)),
        None => out_of_blocked(session, state, p).await,
    }
}

/// Mail the server had in the Blocked folder, which another client renamed
/// or deleted since: by Message-ID in any other folder but Archive and
/// Trash (where it would have gone some other way).
async fn out_of_blocked(
    session: &mut Session,
    state: &SyncState,
    p: &Pending,
) -> Result<Option<(String, u32)>> {
    if p.remote_place != Place::Blocked || p.message_id.is_none() {
        return Ok(None);
    }
    let names: Vec<_> = session
        .list(Some(""), Some("*"))
        .await?
        .try_collect()
        .await?;
    let skip = [
        state.archive.as_deref(),
        state.trash.as_deref(),
        state.blocked.as_deref(),
        p.mailbox.as_deref(),
    ];
    let mailboxes: Vec<String> = names
        .iter()
        .filter(|n| !n.attributes().contains(&NameAttribute::NoSelect))
        .map(|n| n.name().to_owned())
        .filter(|n| !skip.contains(&Some(n.as_str())))
        .collect();
    for mailbox in mailboxes {
        if let Some(found) = search(session, &mailbox, p).await? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

/// Whether a stored UID belongs to the mailbox generation selected now.
fn uid_is_current(
    store: &Mutex<Store>,
    state: &SyncState,
    p: &Pending,
    mailbox: &str,
    actual: Option<u32>,
) -> Result<bool> {
    let expected = if mailbox.eq_ignore_ascii_case(INBOX) {
        state.uid_validity
    } else {
        let store = lock(store);
        let Some(at) = store.location(p.id)? else {
            return Ok(false);
        };
        store
            .folders()?
            .into_iter()
            .find(|f| f.account_id == at.account_id && f.remote_id == mailbox)
            .and_then(|f| serde_json::from_str::<FolderState>(&f.sync_state).ok())
            .and_then(|s| s.uid_validity)
    };
    Ok(expected.is_some() && expected == actual)
}

/// The newest message in `mailbox` with exactly `p`'s Message-ID, leaving
/// the mailbox selected. `None` if it isn't there or the mailbox is gone.
async fn search(
    session: &mut Session,
    mailbox: &str,
    p: &Pending,
) -> Result<Option<(String, u32)>> {
    let Some(message_id) = &p.message_id else {
        return Ok(None);
    };
    match session.select(mailbox).await {
        Ok(_) => {}
        Err(async_imap::error::Error::No(_)) => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    Ok(find_message_id(session, message_id)
        .await?
        .into_iter()
        .max()
        .map(|uid| (mailbox.to_owned(), uid)))
}

/// Exact identities in the selected mailbox. HEADER SEARCH alone matches
/// substrings, so candidates must be checked before destructive commands.
pub(crate) async fn find_message_id(session: &mut Session, message_id: &str) -> Result<Vec<u32>> {
    let query = format!("HEADER Message-ID {}", quoted(message_id));
    let found: Vec<u32> = uid_search(session, query).await?.into_iter().collect();
    if found.is_empty() {
        return Ok(Vec::new());
    }
    let headers: Vec<Fetched> = uid_fetch(
        session,
        uid_set(&found),
        "(UID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])",
    )
    .await?
    .try_collect()
    .await?;
    let expected = parse::bare_id(message_id);
    Ok(headers
        .iter()
        .filter(|f| f.header().and_then(parse::message_id).as_ref() == Some(&expected))
        .filter_map(|f| f.uid)
        .collect())
}

/// What the server can do, for moving mail.
#[derive(Clone, Copy, Debug)]
struct Capabilities {
    can_move: bool,
    uidplus: bool,
    gmail: bool,
}

/// How a message leaves a mailbox for another.
#[derive(Debug, PartialEq, Eq)]
enum Transfer {
    Move,
    /// For servers without MOVE.
    CopyAndExpunge,
    /// Out of Gmail's All Mail, which keeps every message (expunging it
    /// there deletes it): the copy adds the other folder's label.
    Copy,
    /// Neither MOVE nor a scoped expunge is available.
    Unsupported,
}

fn transfer(server: Capabilities, state: &SyncState, from: &str) -> Transfer {
    if server.gmail && state.archive.as_deref() == Some(from) {
        Transfer::Copy
    } else if server.can_move {
        Transfer::Move
    } else if server.uidplus {
        Transfer::CopyAndExpunge
    } else {
        Transfer::Unsupported
    }
}

async fn move_by_copy(
    session: &mut Session,
    uid: u32,
    destination: &str,
) -> async_imap::error::Result<()> {
    session.uid_copy(uid.to_string(), destination).await?;
    let _: Vec<Fetch> = session
        .uid_store(uid.to_string(), "+FLAGS.SILENT (\\Deleted)")
        .await?
        .try_collect()
        .await?;
    let _: Vec<_> = session
        .uid_expunge(uid.to_string())
        .await?
        .try_collect()
        .await?;
    Ok(())
}

/// The mailbox for a place; `None` for a folder, which only the message knows
/// (its home folder).
fn folder(state: &SyncState, place: Place) -> Option<String> {
    match place {
        Place::Inbox => Some(INBOX.into()),
        Place::Archive => Some(state.archive.clone().unwrap_or_else(|| "Archive".into())),
        Place::Trash => Some(state.trash.clone().unwrap_or_else(|| "Trash".into())),
        Place::Blocked => state.blocked.clone(),
        Place::Folder => None,
    }
}

/// (unread, flagged)
fn read_flags(fetch: &Fetched) -> (bool, bool) {
    (!fetch.seen, fetch.flagged)
}

/// What a FETCH response says about a message, as far as syncing reads it:
/// taken out of the response as it's read, header and body copied, and the
/// response dropped.
#[derive(Debug, Default)]
struct Fetched {
    /// The message's sequence number.
    message: u32,
    uid: Option<u32>,
    size: Option<u32>,
    seen: bool,
    flagged: bool,
    header: Option<Vec<u8>>,
    body: Option<Vec<u8>>,
    /// INTERNALDATE, in milliseconds.
    arrived: Option<i64>,
}

impl Fetched {
    fn read(message: u32, attributes: &[AttributeValue<'_>]) -> Self {
        let mut fetched = Self {
            message,
            ..Default::default()
        };
        for attribute in attributes {
            match attribute {
                AttributeValue::Uid(uid) => fetched.uid = Some(*uid),
                AttributeValue::Rfc822Size(size) => fetched.size = Some(*size),
                AttributeValue::Flags(flags) => {
                    for flag in flags {
                        match Flag::from(flag.as_ref()) {
                            Flag::Seen => fetched.seen = true,
                            Flag::Flagged => fetched.flagged = true,
                            _ => {}
                        }
                    }
                }
                // The header, or the fields of it asked for.
                AttributeValue::BodySection {
                    section: Some(SectionPath::Full(MessageSection::Header)),
                    data: Some(data),
                    ..
                }
                | AttributeValue::Rfc822Header(Some(data)) => {
                    fetched.header.get_or_insert_with(|| data.to_vec());
                }
                AttributeValue::BodySection {
                    section: None,
                    data: Some(data),
                    ..
                }
                | AttributeValue::Rfc822(Some(data)) => {
                    fetched.body.get_or_insert_with(|| data.to_vec());
                }
                AttributeValue::InternalDate(date) => fetched.arrived = internal_date(date),
                _ => {}
            }
        }
        fetched
    }

    fn header(&self) -> Option<&[u8]> {
        self.header.as_deref()
    }

    fn body(&self) -> Option<&[u8]> {
        self.body.as_deref()
    }
}

/// An INTERNALDATE ("26-Oct-2026 10:00:00 +0000"), in milliseconds.
fn internal_date(date: &str) -> Option<i64> {
    // A Date header's form, but for the dashes.
    let date = mail_parser::DateTime::parse_rfc822(&date.trim().replacen('-', " ", 2))?;
    date.is_valid().then(|| date.to_timestamp() * 1000)
}

/// FETCH by sequence numbers, read as `run_fetch` reads it.
async fn fetch(
    session: &mut Session,
    set: impl AsRef<str>,
    query: impl AsRef<str>,
) -> Result<impl futures_util::Stream<Item = async_imap::error::Result<Fetched>> + '_> {
    run_fetch(
        session,
        format!("FETCH {} {}", set.as_ref(), query.as_ref()),
    )
    .await
}

/// UID FETCH, read as `run_fetch` reads it.
async fn uid_fetch(
    session: &mut Session,
    set: impl AsRef<str>,
    query: impl AsRef<str>,
) -> Result<impl futures_util::Stream<Item = async_imap::error::Result<Fetched>> + '_> {
    run_fetch(
        session,
        format!("UID FETCH {} {}", set.as_ref(), query.as_ref()),
    )
    .await
}

/// Sends a FETCH and reads what each response says as it comes, up to the
/// command's tagged completion. A NO or BAD there fails the stream: what
/// came before it may be only part of the answer, or none of it, and taken
/// for all of it would lose mail and flag changes for good. Readers that
/// can make do with part of it take a NO as `Completion::Partial`
/// (`next_record`). Responses that aren't FETCH (EXISTS, EXPUNGE) are
/// passed over, as the next SELECT brings the same news. Read to its end,
/// NO included, it leaves the session ready for the next command.
async fn run_fetch(
    session: &mut Session,
    command: String,
) -> Result<impl futures_util::Stream<Item = async_imap::error::Result<Fetched>> + '_> {
    let tag = session.run_command(command).await?;
    Ok(futures_util::stream::try_unfold(session, move |session| {
        let tag = tag.clone();
        async move {
            loop {
                let Some(response) = session.read_response().await? else {
                    return Err(async_imap::error::Error::ConnectionLost);
                };
                match response.parsed() {
                    Response::Fetch(message, attributes) => {
                        return Ok(Some((Fetched::read(*message, attributes), session)));
                    }
                    Response::Done {
                        tag: done,
                        status,
                        outcome,
                    } if *done == tag => return completion(status, outcome).map(|()| None),
                    _ => {}
                }
            }
        }
    }))
}

/// How a FETCH ended, for readers that can make do with part of an answer.
#[derive(Debug, PartialEq, Eq)]
enum Completion {
    /// OK: all of it came.
    Whole,
    /// NO, with what the server said: it couldn't fetch some of the
    /// messages, or any (Gmail: "Some messages could not be FETCHed").
    /// What came is good, but may not be all there is.
    Partial(String),
}

/// A fetch's next record, or (`Break`) how it ended: a NO ends it
/// `Partial`. A BAD, a failed read or a lost connection fails it.
async fn next_record<T>(
    fetched: &mut (impl futures_util::Stream<Item = async_imap::error::Result<T>> + Unpin),
) -> Result<ControlFlow<Completion, T>> {
    Ok(match fetched.try_next().await {
        Ok(Some(record)) => ControlFlow::Continue(record),
        Ok(None) => ControlFlow::Break(Completion::Whole),
        Err(async_imap::error::Error::No(said)) => ControlFlow::Break(Completion::Partial(said)),
        Err(e) => return Err(e.into()),
    })
}

/// Reads a fetch to its end: what came, and how it ended.
async fn read_fetch(
    fetched: impl futures_util::Stream<Item = async_imap::error::Result<Fetched>>,
) -> Result<(Vec<Fetched>, Completion)> {
    let mut fetched = std::pin::pin!(fetched);
    let mut records = Vec::new();
    loop {
        match next_record(&mut fetched).await? {
            ControlFlow::Continue(record) => records.push(record),
            ControlFlow::Break(completion) => return Ok((records, completion)),
        }
    }
}

/// UID FETCH of `uids` in the selected `mailbox`, which a NO holds back
/// only the messages the server couldn't fetch: what came is kept, the
/// rest asked for one at a time, and any it won't give even alone left
/// out. Returns what came, and the UIDs left out. For answers read whole
/// (sizes, headers); a BAD fails it.
async fn uid_fetch_each(
    session: &mut Session,
    mailbox: &str,
    uids: &[u32],
    query: &str,
) -> Result<(Vec<Fetched>, Vec<u32>)> {
    let (mut fetched, completion) =
        read_fetch(uid_fetch(session, uid_set(uids), query).await?).await?;
    let came = fetched.iter().filter_map(|f| f.uid).collect();
    let (again, mut refused) = held_back(mailbox, uids, &came, completion);
    for uid in again {
        let (alone, completion) =
            read_fetch(uid_fetch(session, uid.to_string(), query).await?).await?;
        let came = alone.iter().filter_map(|f| f.uid).collect();
        refused.extend(held_back(mailbox, &[uid], &came, completion).1);
        fetched.extend(alone);
    }
    Ok((fetched, refused))
}

/// Of the `uids` a fetch asked for, with those in `came` back, the ones a
/// NO held back: to ask for again one at a time, and (of a fetch of one,
/// which was that already) refused, left out this round. (After an OK,
/// mail that didn't come was expunged meanwhile.)
fn held_back(
    mailbox: &str,
    uids: &[u32],
    came: &HashSet<u32>,
    completion: Completion,
) -> (Vec<u32>, Vec<u32>) {
    let Completion::Partial(said) = completion else {
        return Default::default();
    };
    let held: Vec<u32> = uids
        .iter()
        .copied()
        .filter(|uid| !came.contains(uid))
        .collect();
    match held[..] {
        [uid] if uids.len() == 1 => {
            skipped(mailbox, &format!("message {uid}"), &said);
            (Vec::new(), held)
        }
        _ => (held, Vec::new()),
    }
}

/// Notes what a server's NO kept from this round, which goes on without it.
fn skipped(mailbox: &str, what: &str, said: &str) {
    eprintln!("spark-rs: {mailbox}: {what} skipped this round; the server said: {said}");
}

/// UID SEARCH. A NO or BAD fails it, where async-imap's search finds
/// nothing: mail a sync can't see would be taken for gone.
async fn uid_search(session: &mut Session, query: impl AsRef<str>) -> Result<HashSet<u32>> {
    let tag = session
        .run_command(format!("UID SEARCH {}", query.as_ref()))
        .await?;
    let mut found = HashSet::new();
    loop {
        let Some(response) = session.read_response().await? else {
            return Err(async_imap::error::Error::ConnectionLost.into());
        };
        match response.parsed() {
            Response::MailboxData(MailboxDatum::Search(uids)) => found.extend(uids),
            Response::Done {
                tag: done,
                status,
                outcome,
            } if *done == tag => {
                completion(status, outcome)?;
                return Ok(found);
            }
            _ => {}
        }
    }
}

/// A command's tagged completion: OK, or the NO or BAD it failed with.
fn completion(status: &Status, outcome: &Outcome<'_>) -> async_imap::error::Result<()> {
    let said = || {
        outcome
            .information
            .as_deref()
            .unwrap_or_default()
            .to_owned()
    };
    match status {
        Status::Ok => Ok(()),
        Status::No => Err(async_imap::error::Error::No(said())),
        Status::Bad => Err(async_imap::error::Error::Bad(said())),
        other => Err(async_imap::error::Error::Io(io::Error::other(format!(
            "{other:?} {}",
            said()
        )))),
    }
}

/// "1:3,7" style set from sorted or unsorted UIDs.
pub(crate) fn uid_set(uids: &[u32]) -> String {
    let mut sorted = uids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i];
        let mut end = start;
        while i + 1 < sorted.len() && sorted[i + 1] == end + 1 {
            i += 1;
            end = sorted[i];
        }
        parts.push(if start == end {
            start.to_string()
        } else {
            format!("{start}:{end}")
        });
        i += 1;
    }
    parts.join(",")
}

pub(crate) fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn lock(store: &Mutex<Store>) -> MutexGuard<'_, Store> {
    store
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod sync_tests;

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

    use super::*;

    #[test]
    fn uid_sets_collapse_runs() {
        assert_eq!(uid_set(&[5, 1, 2, 3, 9, 10]), "1:3,5,9:10");
        assert_eq!(uid_set(&[7]), "7");
    }

    #[test]
    fn body_fetches_keep_to_the_byte_budget() {
        const MIB: u32 = 1024 * 1024;
        let sized =
            |sizes: &[u32]| -> Vec<(u32, u32)> { (1..).zip(sizes.iter().copied()).collect() };
        let lens =
            |sizes: &[u32]| -> Vec<usize> { batches(&sized(sizes)).iter().map(Vec::len).collect() };
        // A hundred messages just under the limit: one a fetch, not 950 MiB at once.
        let big = sized(&[MIB * 19 / 2; 100]);
        let planned = batches(&big);
        assert_eq!(planned.len(), 100);
        assert_eq!(planned.concat(), (1..=100).collect::<Vec<u32>>());
        // Small mail goes FETCH_BATCH at a time, as does mail without a size.
        assert_eq!(lens(&[100 * 1024; 30]), [25, 5]);
        assert_eq!(lens(&[0; 30]), [25, 5]);
        // Mixed sizes share a fetch until the next would pass the budget.
        assert_eq!(
            lens(&[6 * MIB, 6 * MIB, 6 * MIB, 1, 9 * MIB, 7 * MIB]),
            [2, 3, 1]
        );
        assert_eq!(lens(&[1, 20 * MIB, 1]), [1, 1, 1]);
        assert!(batches(&[]).is_empty());
    }

    /// Stands in for a fetched message, counting how many are held at once.
    struct Held {
        n: u32,
        counts: Arc<Counts>,
    }

    #[derive(Default)]
    struct Counts {
        held: AtomicUsize,
        most_held: AtomicUsize,
        parsing: AtomicUsize,
        most_parsing: AtomicUsize,
    }

    impl Held {
        fn new(n: u32, counts: &Arc<Counts>) -> Self {
            let held = counts.held.fetch_add(1, SeqCst) + 1;
            counts.most_held.fetch_max(held, SeqCst);
            Self {
                n,
                counts: counts.clone(),
            }
        }
    }

    impl Drop for Held {
        fn drop(&mut self) {
            self.counts.held.fetch_sub(1, SeqCst);
        }
    }

    /// Every third message can't be parsed.
    fn parse_held(raw: Held) -> Option<Incoming> {
        let parsing = raw.counts.parsing.fetch_add(1, SeqCst) + 1;
        raw.counts.most_parsing.fetch_max(parsing, SeqCst);
        std::thread::sleep(Duration::from_millis(2));
        raw.counts.parsing.fetch_sub(1, SeqCst);
        (!raw.n.is_multiple_of(3)).then(|| Incoming {
            uid: Some(raw.n),
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn fetched_mail_is_parsed_one_message_at_a_time() {
        let counts = Arc::new(Counts::default());
        let fetched = futures_util::stream::iter((1..=20).map(|n| Ok(Held::new(n, &counts))));
        let mut messages = Vec::new();
        let completion = parse_each(fetched, parse_held, &mut messages)
            .await
            .unwrap();
        assert_eq!(completion, Completion::Whole);
        let uids: Vec<u32> = messages.iter().filter_map(|m| m.uid).collect();
        let readable: Vec<u32> = (1..=20u32).filter(|n| !n.is_multiple_of(3)).collect();
        assert_eq!(uids, readable);
        assert_eq!(counts.most_parsing.load(SeqCst), 1);
        assert!(counts.most_held.load(SeqCst) <= 2);
        assert_eq!(counts.held.load(SeqCst), 0);
    }

    #[test]
    fn arrival_times_read_as_imap_writes_them() {
        assert_eq!(
            internal_date("26-Oct-2026 10:00:00 +0000"),
            Some(1_793_008_800_000)
        );
        // A day of one digit is padded with a space.
        assert_eq!(
            internal_date(" 6-Oct-2026 05:00:00 -0500"),
            Some(1_791_280_800_000)
        );
        assert_eq!(internal_date("yesterday"), None);
    }

    #[tokio::test]
    async fn a_connection_lost_mid_fetch_fails_it() {
        let counts = Arc::new(Counts::default());
        let fetched = futures_util::stream::iter((1..=5).map(|n| match n {
            4 => Err(async_imap::error::Error::ConnectionLost),
            n => Ok(Held::new(n, &counts)),
        }));
        let mut messages = Vec::new();
        assert!(
            parse_each(fetched, parse_held, &mut messages)
                .await
                .is_err()
        );
        assert!(counts.most_held.load(SeqCst) <= 2);
    }

    /// A NO keeps the messages that came before it; a BAD fails them all.
    #[tokio::test]
    async fn a_no_mid_fetch_keeps_what_came_and_a_bad_fails_it() {
        let counts = Arc::new(Counts::default());
        let said = "Some messages could not be FETCHed (Failure)";
        let fetched = |end: fn(String) -> async_imap::error::Error| {
            let counts = counts.clone();
            futures_util::stream::iter((1..=5).map(move |n| match n {
                5 => Err(end(said.into())),
                n => Ok(Held::new(n, &counts)),
            }))
        };
        let mut messages = Vec::new();
        let completion = parse_each(
            fetched(async_imap::error::Error::No),
            parse_held,
            &mut messages,
        )
        .await
        .unwrap();
        assert_eq!(completion, Completion::Partial(said.into()));
        let uids: Vec<u32> = messages.iter().filter_map(|m| m.uid).collect();
        assert_eq!(uids, [1, 2, 4]);
        let bad = parse_each(
            fetched(async_imap::error::Error::Bad),
            parse_held,
            &mut messages,
        )
        .await;
        assert!(bad.is_err());
        assert!(counts.most_held.load(SeqCst) <= 2);
    }

    fn listed(name: &str, delimiter: &str, attributes: &[Attr]) -> Listed {
        Listed {
            name: name.into(),
            delimiter: Some(delimiter.into()),
            attributes: attributes.to_vec(),
        }
    }

    #[test]
    fn gmail_folders_get_roles_and_lose_their_container() {
        let state = SyncState {
            archive: Some("[Gmail]/All Mail".into()),
            trash: Some("[Gmail]/Trash".into()),
            ..Default::default()
        };
        let folders = folders_from(
            &[
                listed("INBOX", "/", &[]),
                listed("Travel", "/", &[]),
                listed("Travel/2026", "/", &[]),
                listed("[Gmail]", "/", &[Attr::NoSelect]),
                listed("[Gmail]/All Mail", "/", &[Attr::All]),
                listed("[Gmail]/Drafts", "/", &[Attr::Drafts]),
                listed("[Gmail]/Important", "/", &[]),
                listed("[Gmail]/Sent Mail", "/", &[Attr::Sent]),
                listed("[Gmail]/Spam", "/", &[Attr::Junk]),
                listed("[Gmail]/Starred", "/", &[Attr::Flagged]),
                listed("[Gmail]/Trash", "/", &[Attr::Trash]),
                listed("Entw&APw-rfe", "/", &[]),
            ],
            &state,
        );
        let summary: Vec<(&str, Option<&str>, &str, Option<FolderRole>)> = folders
            .iter()
            .map(|f| {
                (
                    f.remote_id.as_str(),
                    f.parent.as_deref(),
                    f.name.as_str(),
                    f.role,
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("Travel", None, "Travel", None),
                ("Travel/2026", Some("Travel"), "2026", None),
                (
                    "[Gmail]/All Mail",
                    None,
                    "All Mail",
                    Some(FolderRole::Archive)
                ),
                ("[Gmail]/Drafts", None, "Drafts", Some(FolderRole::Drafts)),
                ("[Gmail]/Important", None, "Important", None),
                (
                    "[Gmail]/Sent Mail",
                    None,
                    "Sent Mail",
                    Some(FolderRole::Sent)
                ),
                ("[Gmail]/Spam", None, "Spam", Some(FolderRole::Spam)),
                (
                    "[Gmail]/Starred",
                    None,
                    "Starred",
                    Some(FolderRole::Flagged)
                ),
                ("[Gmail]/Trash", None, "Trash", Some(FolderRole::Trash)),
                ("Entw&APw-rfe", None, "Entwürfe", None),
            ]
        );
    }

    #[test]
    fn blocked_folders_are_found_at_the_top_or_in_the_inbox() {
        let state = SyncState::default();
        let roles = |listed: &[Listed]| -> Vec<(String, Option<FolderRole>)> {
            folders_from(listed, &state)
                .into_iter()
                .map(|f| (f.remote_id, f.role))
                .collect()
        };
        assert_eq!(
            roles(&[
                listed("Blocked", "/", &[]),
                listed("Work/Blocked", "/", &[])
            ]),
            [
                ("Blocked".to_owned(), Some(FolderRole::Blocked)),
                ("Work/Blocked".to_owned(), None)
            ]
        );
        assert_eq!(
            roles(&[listed("INBOX.Blocked", ".", &[])]),
            [("INBOX.Blocked".to_owned(), Some(FolderRole::Blocked))]
        );
        // Once one is known, only that one is.
        let state = SyncState {
            blocked: Some("Spark/Blocked".into()),
            ..Default::default()
        };
        let found = folders_from(
            &[
                listed("Blocked", "/", &[]),
                listed("Spark/Blocked", "/", &[]),
            ],
            &state,
        );
        assert_eq!(
            found.iter().map(|f| f.role).collect::<Vec<_>>(),
            [None, Some(FolderRole::Blocked)]
        );
    }

    #[test]
    fn plain_servers_get_roles_by_name_and_nest_in_the_inbox() {
        let state = SyncState {
            archive: Some("INBOX.Archive".into()),
            trash: Some("INBOX.Trash".into()),
            ..Default::default()
        };
        let folders = folders_from(
            &[
                listed("INBOX", ".", &[]),
                listed("INBOX.Archive", ".", &[]),
                listed("INBOX.Sent", ".", &[]),
                listed("INBOX.Junk", ".", &[]),
                listed("INBOX.Trash", ".", &[]),
                listed("INBOX.Clients", ".", &[Attr::NoSelect]),
                listed("INBOX.Clients.Acme", ".", &[]),
                listed("Gone", ".", &[Attr::NonExistent]),
            ],
            &state,
        );
        let summary: Vec<(&str, Option<&str>, Option<FolderRole>, bool)> = folders
            .iter()
            .map(|f| (f.name.as_str(), f.parent.as_deref(), f.role, f.selectable))
            .collect();
        assert_eq!(
            summary,
            [
                ("Archive", None, Some(FolderRole::Archive), true),
                ("Sent", None, Some(FolderRole::Sent), true),
                ("Junk", None, Some(FolderRole::Spam), true),
                ("Trash", None, Some(FolderRole::Trash), true),
                ("Clients", None, None, false),
                ("Acme", Some("INBOX.Clients"), None, true),
            ]
        );
    }

    #[test]
    fn mail_leaves_gmails_all_mail_by_copy() {
        let gmail = Capabilities {
            can_move: true,
            uidplus: true,
            gmail: true,
        };
        let state = SyncState {
            archive: Some("[Gmail]/All Mail".into()),
            trash: Some("[Gmail]/Trash".into()),
            ..Default::default()
        };
        // Undoing Done: back into a label, or the inbox, as a copy.
        assert_eq!(transfer(gmail, &state, "[Gmail]/All Mail"), Transfer::Copy);
        // From a label or the inbox, a move swaps the labels.
        assert_eq!(transfer(gmail, &state, "Travel"), Transfer::Move);
        assert_eq!(transfer(gmail, &state, INBOX), Transfer::Move);
        // Elsewhere an archive folder is a folder like any other.
        let imap = Capabilities {
            can_move: true,
            uidplus: true,
            gmail: false,
        };
        let state = SyncState {
            archive: Some("Archive".into()),
            ..Default::default()
        };
        assert_eq!(transfer(imap, &state, "Archive"), Transfer::Move);
        let old = Capabilities {
            can_move: false,
            uidplus: true,
            gmail: false,
        };
        assert_eq!(transfer(old, &state, "Travel"), Transfer::CopyAndExpunge);
    }

    #[test]
    fn states_from_before_reaching_back_still_read() {
        let state: SyncState = serde_json::from_str(r#"{"uidValidity":3,"floor":40}"#).unwrap();
        assert_eq!((state.floor, state.complete), (Some(40), false));
        let folder: FolderState = serde_json::from_str(r#"{"uidValidity":3}"#).unwrap();
        assert_eq!((folder.limit, folder.complete), (None, false));
    }

    #[test]
    fn ports_pick_their_usual_security() {
        assert_eq!(Server::new(" imap.gmail.com ", 993).security, Security::Tls);
        assert_eq!(Server::new("mail.x.com", 143).security, Security::StartTls);
        assert_eq!(Server::new("smtp.x.com", 587).security, Security::StartTls);
        assert_eq!(Server::new("smtp.x.com", 465).host, "smtp.x.com");
    }
}
