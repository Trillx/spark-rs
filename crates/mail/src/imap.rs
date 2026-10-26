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

use std::collections::HashSet;
use std::io;
use std::pin::Pin;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::task::{Context, Poll};
use std::time::Duration;

use async_imap::imap_proto::NameAttribute;
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
    }

    // The newest messages by sequence number, with their flags.
    let limit = state.limit.unwrap_or(FOLDER_LIMIT);
    state.complete = mailbox.exists <= limit;
    let mut window: Vec<(u32, bool, bool)> = Vec::new();
    let mut header_changes = 0;
    if mailbox.exists > 0 {
        let first = mailbox.exists.saturating_sub(limit) + 1;
        let fetched: Vec<Fetch> = session
            .fetch(
                format!("{first}:*"),
                "(UID FLAGS BODY.PEEK[HEADER.FIELDS (MESSAGE-ID REPLY-TO)])",
            )
            .await?
            .try_collect()
            .await?;
        for f in &fetched {
            if let Some(uid) = f.uid {
                let (unread, flagged) = read_flags(f);
                window.push((uid, unread, flagged));
                if let Some(headers) = f.header() {
                    header_changes += usize::from(lock(store).observe_reply_to(&Incoming {
                        account_id: folder.account_id,
                        mailbox: Some(folder.remote_id.clone()),
                        uid: Some(uid),
                        message_id: parse::message_id(headers),
                        reply_to: parse::reply_to(headers),
                        ..Default::default()
                    })?);
                }
            }
        }
    }

    let known = lock(store).folder_uids(folder.id)?;
    let mut report = FolderReport {
        changed: header_changes,
        ..Default::default()
    };
    let mut members: Vec<(i64, Option<u32>)> = Vec::new();
    {
        let store = lock(store);
        for (uid, unread, flagged) in &window {
            if let Some(id) = known.get(uid) {
                store.observe_flags(
                    folder.account_id,
                    &folder.remote_id,
                    *uid,
                    *unread,
                    *flagged,
                )?;
                members.push((*id, Some(*uid)));
            }
        }
    }

    let new: Vec<u32> = window
        .iter()
        .map(|(uid, ..)| *uid)
        .filter(|uid| !known.contains_key(uid))
        .collect();
    for batch in new.chunks(FETCH_BATCH * 4) {
        let heads: Vec<Fetch> = session
            .uid_fetch(
                uid_set(batch),
                "(UID FLAGS BODY.PEEK[HEADER.FIELDS (MESSAGE-ID REPLY-TO)])",
            )
            .await?
            .try_collect()
            .await?;
        let mut unknown = Vec::new();
        for f in &heads {
            let Some(uid) = f.uid else { continue };
            let message_id = f.header().and_then(parse::message_id);
            let stored = match &message_id {
                Some(id) => lock(store).has_message_id(folder.account_id, id)?,
                None => false,
            };
            if !stored {
                unknown.push(uid);
                continue;
            }
            let (unread, flagged) = read_flags(f);
            let m = Incoming {
                account_id: folder.account_id,
                mailbox: Some(folder.remote_id.clone()),
                uid: Some(uid),
                message_id,
                reply_to: f.header().and_then(parse::reply_to),
                unread,
                flagged,
                ..Default::default()
            };
            members.push((
                added_id(lock(store).add_folder_message(folder.id, &m)?),
                Some(uid),
            ));
            report.changed += 1;
        }
        for mut m in fetch_messages(session, &unknown, &folder.remote_id).await? {
            m.account_id = folder.account_id;
            let uid = m.uid;
            members.push((
                added_id(lock(store).add_folder_message(folder.id, &m)?),
                uid,
            ));
            report.changed += 1;
        }
    }
    report.changed += lock(store).set_folder_members(folder.id, &members)?;
    Ok(report)
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
/// mail to them. Addresses that can't be searched for are left out.
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
        if !session
            .uid_search(format!("OR OR TO {q} CC {q} BCC {q}"))
            .await?
            .is_empty()
        {
            found.push(address.clone());
        }
    }
    Ok(found)
}

/// Brings the inbox into the store: new messages, flag changes, and
/// messages that left the inbox elsewhere.
pub async fn sync_inbox(
    session: &mut Session,
    store: &Mutex<Store>,
    account_id: i64,
    state: &mut SyncState,
) -> Result<SyncReport> {
    let mailbox = session.select(INBOX).await?;
    if mailbox.uid_validity != state.uid_validity {
        // The server renumbered the inbox. Stored messages are found again
        // by Message-ID as they're fetched.
        if state.uid_validity.is_some() {
            lock(store).forget_uids(account_id, INBOX)?;
        }
        state.uid_validity = mailbox.uid_validity;
        state.floor = None;
    }

    let present = session.uid_search("ALL").await?;
    let first = state.floor.is_none();
    let floor = match state.floor {
        Some(floor) => floor,
        None => {
            let mut newest: Vec<u32> = present.iter().copied().collect();
            newest.sort_unstable_by(|a, b| b.cmp(a));
            newest.truncate(FIRST_SYNC_LIMIT);
            newest
                .last()
                .copied()
                .unwrap_or_else(|| mailbox.uid_next.unwrap_or(1))
        }
    };
    let complete = present.iter().all(|uid| *uid >= floor);
    let present: HashSet<u32> = present.into_iter().filter(|uid| *uid >= floor).collect();
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

    // Flags of everything else we track in the inbox.
    if !known.is_empty() {
        let flags: Vec<Fetch> = session
            .uid_fetch(
                format!("{floor}:*"),
                "(UID FLAGS BODY.PEEK[HEADER.FIELDS (MESSAGE-ID REPLY-TO)])",
            )
            .await?
            .try_collect()
            .await?;
        let store = lock(store);
        for fetch in &flags {
            if let Some(uid) = fetch.uid.filter(|uid| known.contains(uid)) {
                let (unread, flagged) = read_flags(fetch);
                store.observe_flags(account_id, INBOX, uid, unread, flagged)?;
                if let Some(headers) = fetch.header() {
                    report.headers += usize::from(store.observe_reply_to(&Incoming {
                        account_id,
                        mailbox: Some(INBOX.into()),
                        uid: Some(uid),
                        message_id: parse::message_id(headers),
                        reply_to: parse::reply_to(headers),
                        ..Default::default()
                    })?);
                }
            }
        }
    }

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
    let present = session.uid_search("ALL").await?;
    let known = lock(store).uids(account_id, INBOX)?;
    let mut below: Vec<u32> = present.iter().copied().filter(|uid| *uid < floor).collect();
    below.sort_unstable_by(|a, b| b.cmp(a));
    // Down to the `OLDER_STEP`th one not stored yet: mail a search stored
    // is passed, not fetched again.
    let mut older = Vec::new();
    let mut unknown = 0;
    for uid in below {
        if !known.contains(&uid) {
            if unknown == OLDER_STEP {
                break;
            }
            unknown += 1;
        }
        older.push(uid);
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
        // Lowered batch by batch (they're newest first), so mail stored
        // before a failure is above the floor, where syncs track it.
        state.floor = batch.last().copied();
    }
    let floor = state.floor.unwrap_or(floor);
    state.complete = present.iter().all(|uid| *uid >= floor);
    Ok(stored)
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
        let hits = newest(session.uid_search(&keys).await?, |uid| {
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
    let hits = newest(session.uid_search(&keys).await?, |uid| {
        !known.contains_key(&uid)
    });
    for batch in hits.chunks(FETCH_BATCH) {
        // Mail stored from elsewhere (the inbox, another label) is left as it is.
        let heads: Vec<Fetch> = session
            .uid_fetch(
                uid_set(batch),
                "(UID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])",
            )
            .await?
            .try_collect()
            .await?;
        let mut unknown = Vec::new();
        for f in &heads {
            let Some(uid) = f.uid else { continue };
            let stored_already = match f.header().and_then(parse::message_id) {
                Some(id) => lock(store).has_message_id(account.id, &id)?,
                None => false,
            };
            if !stored_already {
                unknown.push(uid);
            }
        }
        for mut m in fetch_messages(session, &unknown, &folder.remote_id).await? {
            m.account_id = account.id;
            if let Added::New(_) = lock(store).add_folder_message(folder.id, &m)? {
                stored += 1;
            }
        }
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

async fn fetch_new(
    session: &mut Session,
    store: &Mutex<Store>,
    account_id: i64,
    uids: &[u32],
    screen_new_senders: bool,
) -> Result<Vec<i64>> {
    let mut added = Vec::new();
    for mut m in fetch_messages(session, uids, INBOX).await? {
        m.account_id = account_id;
        if let Added::New(id) = lock(store).add_incoming(&m, screen_new_senders)? {
            added.push(id);
        }
    }
    Ok(added)
}

/// Fetches and parses messages from the selected `mailbox`, with their
/// location, flags and arrival time; big ones without their body. The
/// account is left for the caller.
async fn fetch_messages(
    session: &mut Session,
    uids: &[u32],
    mailbox: &str,
) -> Result<Vec<Incoming>> {
    if uids.is_empty() {
        return Ok(Vec::new());
    }
    let sizes: Vec<Fetch> = session
        .uid_fetch(uid_set(uids), "(UID RFC822.SIZE)")
        .await?
        .try_collect()
        .await?;
    let (small, large): (Vec<_>, Vec<_>) = sizes
        .iter()
        .filter_map(|f| Some((f.uid?, f.size.unwrap_or(0))))
        .partition(|(_, size)| *size <= MAX_BODY_BYTES);

    let mut messages = Vec::new();
    for (uids, query) in [
        (small, "(UID FLAGS INTERNALDATE BODY.PEEK[])"),
        (large, "(UID FLAGS INTERNALDATE BODY.PEEK[HEADER])"),
    ] {
        if uids.is_empty() {
            continue;
        }
        let uids: Vec<u32> = uids.into_iter().map(|(uid, _)| uid).collect();
        let fetched: Vec<Fetch> = session
            .uid_fetch(uid_set(&uids), query)
            .await?
            .try_collect()
            .await?;
        for fetch in &fetched {
            let Some(uid) = fetch.uid else { continue };
            let raw = fetch.body().or_else(|| fetch.header()).unwrap_or_default();
            let Some(mut m) = parse::parse_message(raw) else {
                continue;
            };
            if fetch.body().is_none() {
                m.text = "This message is too large to show here.".into();
                m.snippet = m.text.clone();
            }
            let (unread, flagged) = read_flags(fetch);
            m.mailbox = Some(mailbox.into());
            m.uid = Some(uid);
            m.unread = unread;
            m.flagged = flagged;
            if let Some(date) = fetch.internal_date() {
                m.date = date.timestamp_millis();
            }
            messages.push(m);
        }
    }
    Ok(messages)
}

/// Sends local changes to the server: flags (read, pinned as flagged), then
/// the move for Done, Delete, blocking, filing into a folder, or back to the
/// inbox. Returns how many messages were updated. A message the server no
/// longer has is skipped for good. Records the Blocked folder in `state`
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
                Err(async_imap::error::Error::No(_)) => {
                    lock(store).drop_pending(p.id)?;
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
    if let (Some(mailbox), Some(uid)) = (&at.mailbox, at.uid) {
        session.select(mailbox).await?;
        if let Some(raw) = fetch_body(session, uid).await? {
            return Ok(Some(raw));
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
        let found = session.uid_search(query).await?;
        if let Some(uid) = found.into_iter().max()
            && let Some(raw) = fetch_body(session, uid).await?
        {
            return Ok(Some(raw));
        }
    }
    Ok(None)
}

async fn fetch_body(session: &mut Session, uid: u32) -> Result<Option<Vec<u8>>> {
    let fetched: Vec<Fetch> = session
        .uid_fetch(uid.to_string(), "(UID BODY.PEEK[])")
        .await?
        .try_collect()
        .await?;
    Ok(fetched.iter().find_map(|f| f.body().map(<[u8]>::to_vec)))
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
        return Ok(None);
    };
    search(session, &mailbox, p).await
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
    let found: Vec<u32> = session.uid_search(query).await?.into_iter().collect();
    if found.is_empty() {
        return Ok(Vec::new());
    }
    let headers: Vec<Fetch> = session
        .uid_fetch(
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
fn read_flags(fetch: &Fetch) -> (bool, bool) {
    let mut seen = false;
    let mut flagged = false;
    for flag in fetch.flags() {
        match flag {
            Flag::Seen => seen = true,
            Flag::Flagged => flagged = true,
            _ => {}
        }
    }
    (!seen, flagged)
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
mod tests {
    use super::*;

    #[test]
    fn uid_sets_collapse_runs() {
        assert_eq!(uid_set(&[5, 1, 2, 3, 9, 10]), "1:3,5,9:10");
        assert_eq!(uid_set(&[7]), "7");
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
