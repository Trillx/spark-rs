//! IMAP: connecting, finding the archive and trash folders, syncing the
//! inbox into the store, and pushing local changes back.
//!
//! Only the inbox is synced. The first sync takes the newest messages (up to
//! a limit) and remembers the lowest UID taken as the floor; later syncs look
//! at everything from the floor up, so new mail and mail moved back into the
//! inbox both show up, while older mail stays on the server.

use std::collections::HashSet;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use async_imap::imap_proto::NameAttribute;
use async_imap::types::{Fetch, Flag};
use async_imap::{Client, Session as ImapSession};
use futures_util::TryStreamExt;
use rustls::pki_types::ServerName;
use serde::{Deserialize, Serialize};
use spark_store::{Added, Location, Pending, Place, Pushed, Store};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::client::TlsStream;

use crate::{Error, Result, parse};

pub const INBOX: &str = "INBOX";
/// Messages fetched on the first sync of an account.
pub const FIRST_SYNC_LIMIT: usize = 300;
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

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub new: usize,
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
        report.new += fetch_new(session, store, account_id, batch, !first).await?;
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
) -> Result<usize> {
    let set = uid_set(uids);
    let sizes: Vec<Fetch> = session
        .uid_fetch(&set, "(UID RFC822.SIZE)")
        .await?
        .try_collect()
        .await?;
    let (small, large): (Vec<_>, Vec<_>) = sizes
        .iter()
        .filter_map(|f| Some((f.uid?, f.size.unwrap_or(0))))
        .partition(|(_, size)| *size <= MAX_BODY_BYTES);

    let mut added = 0;
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
            m.account_id = account_id;
            m.mailbox = Some(INBOX.into());
            m.uid = Some(uid);
            m.unread = unread;
            m.flagged = flagged;
            if let Some(date) = fetch.internal_date() {
                m.date = date.timestamp_millis();
            }
            if let Added::New(_) = lock(store).add_incoming(&m, screen_new_senders)? {
                added += 1;
            }
        }
    }
    Ok(added)
}

/// Sends local changes to the server: flags (read, pinned as flagged), then
/// the move for Done or Delete. Returns how many messages were updated.
/// A message the server no longer has is skipped for good.
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
    let can_move = caps.has_str("MOVE");
    let mut selected: Option<String> = None;
    let mut pushed = 0;
    for p in pending {
        // Where is it now? Mail that left the inbox is found by Message-ID.
        let found = match (&p.mailbox, p.uid) {
            (Some(mailbox), Some(uid)) => Some((mailbox.clone(), uid)),
            _ => locate(session, state, &p, &mut selected).await?,
        };
        let Some((mailbox, uid)) = found else {
            lock(store).drop_pending(p.id)?;
            continue;
        };
        if selected.as_deref() != Some(mailbox.as_str()) {
            session.select(&mailbox).await?;
            selected = Some(mailbox.clone());
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

        let destination = folder(state, p.place);
        let moved = if destination != mailbox {
            let done = if can_move {
                session.uid_mv(uid.to_string(), &destination).await
            } else {
                move_by_copy(session, uid, &destination).await
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
        // After a move the new UID isn't known; the next sync finds the
        // message again by Message-ID if it's back in the inbox.
        let (at_mailbox, at_uid) = if moved {
            ((p.place == Place::Inbox).then_some(INBOX), None)
        } else {
            (
                (mailbox == INBOX).then_some(INBOX),
                (mailbox == INBOX).then_some(uid),
            )
        };
        lock(store).mark_pushed(
            &p,
            Pushed {
                mailbox: at_mailbox,
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
    let mut folders = vec![folder(state, at.remote_place)];
    if folders[0] != INBOX {
        folders.push(INBOX.into());
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

async fn locate(
    session: &mut Session,
    state: &SyncState,
    p: &Pending,
    selected: &mut Option<String>,
) -> Result<Option<(String, u32)>> {
    let Some(message_id) = &p.message_id else {
        return Ok(None);
    };
    let mailbox = folder(state, p.remote_place);
    session.select(&mailbox).await?;
    *selected = Some(mailbox.clone());
    // HEADER search matches substrings, so the bare id finds it with or without brackets.
    let query = format!("HEADER Message-ID {}", quoted(message_id));
    let found = session.uid_search(query).await?;
    Ok(found.into_iter().max().map(|uid| (mailbox, uid)))
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

fn folder(state: &SyncState, place: Place) -> String {
    match place {
        Place::Inbox => INBOX.into(),
        Place::Archive => state.archive.clone().unwrap_or_else(|| "Archive".into()),
        Place::Trash => state.trash.clone().unwrap_or_else(|| "Trash".into()),
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
fn uid_set(uids: &[u32]) -> String {
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

fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
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
    fn uid_sets_collapse_runs() {
        assert_eq!(uid_set(&[5, 1, 2, 3, 9, 10]), "1:3,5,9:10");
        assert_eq!(uid_set(&[7]), "7");
    }

    #[test]
    fn ports_pick_their_usual_security() {
        assert_eq!(Server::new(" imap.gmail.com ", 993).security, Security::Tls);
        assert_eq!(Server::new("mail.x.com", 143).security, Security::StartTls);
        assert_eq!(Server::new("smtp.x.com", 587).security, Security::StartTls);
        assert_eq!(Server::new("smtp.x.com", 465).host, "smtp.x.com");
    }
}
