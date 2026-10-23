//! IMAP: connecting, finding the archive and trash folders, syncing the
//! inbox into the store, listing and syncing other folders, and pushing
//! local changes back.
//!
//! The first inbox sync takes the newest messages (up to a limit) and
//! remembers the lowest UID taken as the floor; later syncs look at
//! everything from the floor up, so new mail and mail moved back into the
//! inbox both show up, while older mail stays on the server. Other folders
//! are synced only when the user wants them, and only their newest messages.

use std::collections::HashSet;
use std::io;
use std::pin::Pin;
use std::sync::MutexGuard;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use async_imap::imap_proto::NameAttribute;
use async_imap::types::{Fetch, Flag};
use async_imap::{Client, Session as ImapSession};
use futures_util::TryStreamExt;
use rustls::pki_types::ServerName;
use serde::{Deserialize, Serialize};
use spark_store::{
    Added, Folder, FolderRole, Incoming, Location, Pending, Place, Pushed, RemoteFolder, Store,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::client::TlsStream;

use crate::{Error, Result, parse, utf7};

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
}

impl Server {
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.trim().to_owned(),
            port,
            security: Security::for_port(port),
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
    pub archive: Option<String>,
    pub trash: Option<String>,
}

/// Where a folder's sync stands, kept as JSON with the folder.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderState {
    pub uid_validity: Option<u32>,
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
            greeted(Client::new(Stream::Tls(Box::new(
                tls(&server.host, tcp).await?,
            ))))
            .await?
        }
        Security::StartTls => {
            let mut client = greeted(Client::new(Stream::Plain(tcp))).await?;
            client.run_command_and_check_ok("STARTTLS", None).await?;
            let Stream::Plain(tcp) = client.into_inner() else {
                unreachable!("STARTTLS starts from a plain stream")
            };
            // No second greeting after STARTTLS.
            Client::new(Stream::Tls(Box::new(tls(&server.host, tcp).await?)))
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

pub(crate) fn tls_config() -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("aws-lc-rs supports the default TLS versions")
    .with_root_certificates(roots)
    .with_no_client_auth();
    Arc::new(config)
}

async fn tls(host: &str, tcp: TcpStream) -> Result<TlsStream<TcpStream>> {
    let name = ServerName::try_from(host.to_owned()).map_err(|e| Error::network(host, e))?;
    timeout(
        NETWORK_TIMEOUT,
        tokio_rustls::TlsConnector::from(tls_config()).connect(name, tcp),
    )
    .await
    .map_err(|_| Error::network(host, "TLS handshake timed out"))?
    .map_err(|e| Error::network(host, e))
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

/// Brings a folder's newest messages (up to `FOLDER_LIMIT`) into the store
/// and makes them its list. Mail already stored (found by Message-ID) isn't
/// downloaded again.
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
    let mut window: Vec<(u32, bool, bool)> = Vec::new();
    if mailbox.exists > 0 {
        let first = mailbox.exists.saturating_sub(FOLDER_LIMIT) + 1;
        let fetched: Vec<Fetch> = session
            .fetch(format!("{first}:*"), "(UID FLAGS)")
            .await?
            .try_collect()
            .await?;
        for f in &fetched {
            if let Some(uid) = f.uid {
                let (unread, flagged) = read_flags(f);
                window.push((uid, unread, flagged));
            }
        }
    }

    let known = lock(store).folder_uids(folder.id)?;
    let mut report = FolderReport::default();
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
                "(UID FLAGS BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])",
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

/// The hierarchy delimiter the server uses at `mailbox`.
async fn delimiter(session: &mut Session, mailbox: &str) -> Result<Option<String>> {
    let names: Vec<_> = session
        .list(Some(""), Some(mailbox))
        .await?
        .try_collect()
        .await?;
    Ok(names.iter().find_map(|n| n.delimiter().map(str::to_owned)))
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
            .uid_fetch(format!("{floor}:*"), "(UID FLAGS)")
            .await?
            .try_collect()
            .await?;
        let store = lock(store);
        for fetch in &flags {
            if let Some(uid) = fetch.uid.filter(|uid| known.contains(uid)) {
                let (unread, flagged) = read_flags(fetch);
                store.observe_flags(account_id, INBOX, uid, unread, flagged)?;
            }
        }
    }

    let gone: Vec<u32> = known.difference(&present).copied().collect();
    if !gone.is_empty() {
        report.gone = lock(store).observe_gone_uids(account_id, INBOX, &gone)?;
    }
    state.floor = Some(floor);
    Ok(report)
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
/// the move for Done, Delete, filing into a folder, or back to the inbox.
/// Returns how many messages were updated. A message the server no longer
/// has is skipped for good.
pub async fn push(
    session: &mut Session,
    store: &Mutex<Store>,
    state: &SyncState,
    pending: Vec<Pending>,
) -> Result<usize> {
    if pending.is_empty() {
        return Ok(0);
    }
    let caps = session.capabilities().await?;
    let server = Capabilities {
        can_move: caps.has_str("MOVE"),
        gmail: caps.has_str("X-GM-EXT-1"),
    };
    let mut selected: Option<String> = None;
    let mut pushed = 0;
    for p in pending {
        // Where is it now? At its known location if that folder still
        // exists; else found by Message-ID (mail that left the inbox), or
        // for filed mail, in a folder that lists it.
        let mut found = match (&p.mailbox, p.uid) {
            (Some(mailbox), Some(uid)) => Some((mailbox.clone(), uid)),
            _ => None,
        };
        if let Some((mailbox, _)) = &found
            && selected.as_deref() != Some(mailbox.as_str())
        {
            match session.select(mailbox).await {
                Ok(_) => selected = Some(mailbox.clone()),
                // Renamed or deleted elsewhere.
                Err(async_imap::error::Error::No(_)) => {
                    selected = None;
                    found = None;
                }
                Err(e) => return Err(e.into()),
            }
        }
        if found.is_none() {
            found = locate(session, store, state, &p, &mut selected).await?;
        }
        let Some((mailbox, uid)) = found else {
            lock(store).drop_pending(p.id)?;
            continue;
        };

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
        let destination = folder(state, p.place)
            .or_else(|| p.folder.as_ref().map(|(_, home)| home.clone()))
            .unwrap_or_else(|| mailbox.clone());
        let moved = if destination != mailbox {
            let done = match transfer(server, state, &mailbox) {
                Transfer::Move => session.uid_mv(uid.to_string(), &destination).await,
                Transfer::CopyAndExpunge => move_by_copy(session, uid, &destination).await,
                Transfer::Copy => session.uid_copy(uid.to_string(), &destination).await,
            };
            match done {
                Ok(()) => true,
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
    selected: &mut Option<String>,
) -> Result<Option<(String, u32)>> {
    if let Some(mailbox) = &p.mailbox
        && let Some(found) = search(session, mailbox, p, selected).await?
    {
        return Ok(Some(found));
    }
    let Some(mailbox) = folder(state, p.remote_place) else {
        let candidates = lock(store).member_locations(p.id)?;
        for (mailbox, uid) in candidates {
            match session.select(&mailbox).await {
                Ok(_) => {
                    *selected = Some(mailbox.clone());
                    return Ok(Some((mailbox, uid)));
                }
                Err(async_imap::error::Error::No(_)) => *selected = None,
                Err(e) => return Err(e.into()),
            }
        }
        return Ok(None);
    };
    search(session, &mailbox, p, selected).await
}

/// The newest message in `mailbox` with `p`'s Message-ID, leaving the
/// mailbox selected. `None` if it isn't there or the mailbox is gone.
async fn search(
    session: &mut Session,
    mailbox: &str,
    p: &Pending,
    selected: &mut Option<String>,
) -> Result<Option<(String, u32)>> {
    let Some(message_id) = &p.message_id else {
        return Ok(None);
    };
    match session.select(mailbox).await {
        Ok(_) => *selected = Some(mailbox.to_owned()),
        Err(async_imap::error::Error::No(_)) => {
            *selected = None;
            return Ok(None);
        }
        Err(e) => return Err(e.into()),
    }
    // HEADER search matches substrings, so the bare id finds it with or without brackets.
    let query = format!("HEADER Message-ID {}", quoted(message_id));
    let found = session.uid_search(query).await?;
    Ok(found.into_iter().max().map(|uid| (mailbox.to_owned(), uid)))
}

/// What the server can do, for moving mail.
#[derive(Clone, Copy, Debug)]
struct Capabilities {
    can_move: bool,
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
}

fn transfer(server: Capabilities, state: &SyncState, from: &str) -> Transfer {
    if server.gmail && state.archive.as_deref() == Some(from) {
        Transfer::Copy
    } else if server.can_move {
        Transfer::Move
    } else {
        Transfer::CopyAndExpunge
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
    let _: Vec<_> = session.expunge().await?.try_collect().await?;
    Ok(())
}

/// The mailbox for a place; `None` for a folder, which only the message knows
/// (its home folder).
fn folder(state: &SyncState, place: Place) -> Option<String> {
    match place {
        Place::Inbox => Some(INBOX.into()),
        Place::Archive => Some(state.archive.clone().unwrap_or_else(|| "Archive".into())),
        Place::Trash => Some(state.trash.clone().unwrap_or_else(|| "Trash".into())),
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
            gmail: false,
        };
        let state = SyncState {
            archive: Some("Archive".into()),
            ..Default::default()
        };
        assert_eq!(transfer(imap, &state, "Archive"), Transfer::Move);
        let old = Capabilities {
            can_move: false,
            gmail: false,
        };
        assert_eq!(transfer(old, &state, "Travel"), Transfer::CopyAndExpunge);
    }

    #[test]
    fn ports_pick_their_usual_security() {
        assert_eq!(Server::new(" imap.gmail.com ", 993).security, Security::Tls);
        assert_eq!(Server::new("mail.x.com", 143).security, Security::StartTls);
        assert_eq!(Server::new("smtp.x.com", 587).security, Security::StartTls);
        assert_eq!(Server::new("smtp.x.com", 465).host, "smtp.x.com");
    }
}
