//! Server drafts. What the composer saves is queued in the store
//! (`Store::save_draft`) and shows in Drafts at once; a sync round uploads
//! it to the account's Drafts folder once the save has settled, replacing
//! the copy it had there, and deletes copies that were replaced, discarded
//! or sent. IMAP (Gmail too) appends a new copy and expunges the old one;
//! Graph updates a draft in place, or makes it anew when it has files.

use std::sync::{Mutex, MutexGuard};

use async_imap::imap_proto::NameAttribute;
use async_imap::types::{Fetch, Name};
use futures_util::TryStreamExt;
use lettre::address::Envelope;
use lettre::message::Mailbox;
use lettre::message::header::{HeaderName, HeaderValue};
use spark_store::{
    Account, Attachment, Contact, DraftCopy, DraftFile, DraftSave, DraftUpload, FolderRole,
    Location, OutboxFile, Store,
};

use crate::attachment::{self, AttachmentData, mime_for};
use crate::draft::{self, Draft, Original, OutgoingAttachment};
use crate::engine::Engine;
use crate::graph::Graph;
use crate::imap::{self, Session, SyncState};
use crate::signature::{self, BODY_SECTION, SIGNATURE_SECTION};
use crate::{Error, Result, parse, trackers};

/// What a round did with the account's drafts.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DraftsReport {
    /// Drafts uploaded, and server copies deleted.
    pub uploaded: usize,
    pub removed: usize,
    /// The Drafts folder had to be found or made, and the folders listed.
    pub folders_changed: bool,
    /// Drafts the server refused, which stay on this computer only.
    pub errors: Vec<String>,
}

impl Engine {
    /// A saved draft for the composer; `None` if `id` isn't a draft.
    pub fn open_draft(&self, id: i64) -> Result<Option<Draft>> {
        open(&self.store(), id)
    }
}

/// Saves the composer's draft as of `now` (see `Store::save_draft`), with
/// its files from this computer in order; the account's next rounds put it
/// on the server. Returns the draft's message id.
pub fn save(
    store: &mut Store,
    account: &Account,
    draft: &Draft,
    files: &[DraftFile],
    now: i64,
) -> Result<i64> {
    save_with(store, account, draft, files, now, None)
}

/// Saves the failed email before its outbox copy is removed.
pub(crate) fn recover_failed_send(
    store: &mut Store,
    account: &Account,
    draft: &Draft,
    files: &[OutboxFile],
    now: i64,
    outbox_id: i64,
) -> Result<i64> {
    let files: Vec<DraftFile> = files.iter().map(DraftFile::Copy).collect();
    save_with(store, account, draft, &files, now, Some(outbox_id))
}

fn save_with(
    store: &mut Store,
    account: &Account,
    draft: &Draft,
    files: &[DraftFile],
    now: i64,
    outbox_id: Option<i64>,
) -> Result<i64> {
    let threading = threading(store, draft)?;
    let saved = Draft {
        draft_id: None,
        send_at: 0,
        ..draft.clone()
    };
    let json = serde_json::to_string(&saved).expect("drafts serialize");
    let attachments: Vec<Attachment> = draft
        .attachments
        .iter()
        .map(|a| match a {
            OutgoingAttachment::File { name, size, .. }
            | OutgoingAttachment::Message { name, size, .. } => Attachment {
                name: name.clone(),
                mime: mime_for(name).into(),
                size: *size,
                remote_id: None,
            },
        })
        .collect();
    // What it says in full, as it would on the server.
    let bodies = draft.saved_bodies();
    let from_name = if account.name.is_empty() {
        &account.email
    } else {
        &account.name
    };
    let send_later = draft
        .send_later
        .as_ref()
        .map(|later| serde_json::to_string(later).expect("Send Later metadata serializes"));
    let save = DraftSave {
        account_id: account.id,
        draft: &json,
        send_later: send_later.as_deref(),
        from_name,
        from_address: &account.email,
        to: &display_list(&draft.to),
        cc: &display_list(&draft.cc),
        bcc: &display_list(&draft.bcc),
        subject: draft.subject.trim(),
        snippet: &parse::snippet(&bodies.text),
        text: &bodies.text,
        html: bodies.html.as_deref(),
        in_reply_to: threading.as_ref().map(|(id, _)| id.as_str()),
        references: threading.as_ref().map(|(_, refs)| refs.as_str()),
        attachments: &attachments,
        files,
    };
    Ok(match outbox_id {
        Some(id) => store.save_failed_send(id, draft.draft_id, &save, now)?,
        None => store.save_draft(draft.draft_id, &save, now)?,
    })
}

/// A saved draft for the composer: as last saved here while it waits to go
/// up, else from its stored copy, its attachments taken from there when
/// it's sent or saved again. `None` if `id` isn't a draft.
pub fn open(store: &Store, id: i64) -> Result<Option<Draft>> {
    if !store.is_draft(id)? {
        return Ok(None);
    }
    if let Some(json) = store.draft(id)?
        && let Ok(mut draft) = serde_json::from_str::<Draft>(&json)
    {
        draft.draft_id = Some(id);
        return Ok(Some(draft));
    }
    let (Some(m), Some(content), Some(headers)) =
        (store.message(id)?, store.content(id)?, store.headers(id)?)
    else {
        return Ok(None);
    };
    let text = content.text.replace("\r\n", "\n");
    let in_reply_to = store.in_reply_to(id)?;
    // A reply to mail stored here is quoted when it's sent, like one written
    // here; one written elsewhere usually has the quote in its text already,
    // and only threads.
    let quoted = text.lines().any(|line| line.trim_start().starts_with('>'));
    let reply_to = match &in_reply_to {
        Some(original) if !quoted => store
            .find_message_id(m.account_id, original)?
            .filter(|original| *original != id),
        _ => None,
    };
    let references = in_reply_to.as_ref().and_then(|original| {
        let refs: Vec<&str> = headers
            .references
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .filter(|r| r != original)
            .collect();
        (!refs.is_empty()).then(|| refs.join(" "))
    });
    let threads = reply_to.is_none();
    let parts = stored_parts(store, &text, content.html.as_deref())?;
    Ok(Some(Draft {
        account: m.account_id,
        to: field(&content.to),
        cc: field(&content.cc),
        bcc: field(&content.bcc),
        subject: m.subject,
        body: parts.body,
        body_html: parts.body_html,
        reply_to,
        send_at: 0,
        send_later: store
            .draft_send_later(id)?
            .map(|json| serde_json::from_str(&json))
            .transpose()
            .map_err(|e| {
                Error::Config(format!("Couldn't read the draft's Send Later time: {e}"))
            })?,
        attachments: content
            .attachments
            .iter()
            .enumerate()
            .map(|(index, a)| OutgoingAttachment::Message {
                message: id,
                index,
                name: a.name.clone(),
                size: a.size,
            })
            .collect(),
        draft_id: Some(id),
        in_reply_to: in_reply_to.filter(|_| threads),
        references: references.filter(|_| threads),
        signature: parts.signature,
        signature_id: parts.signature_id,
        quote: parts.quote,
        // Saved drafts don't keep a reminder; it's chosen when sending.
        remind: None,
    }))
}

/// The line a forward's original starts with (the composer writes it).
const FORWARDED: &str = "---------- Forwarded message ----------";

/// A stored draft's text, split around its signature.
#[derive(Debug, Default, PartialEq)]
struct StoredParts {
    body: String,
    /// The body's formatting, when the draft has HTML.
    body_html: Option<String>,
    signature: Option<String>,
    signature_id: Option<i64>,
    /// A forward's original, below the signature.
    quote: Option<String>,
}

/// Splits a stored draft's text around the signature its HTML has, if any.
/// One whose text matches a saved signature comes back as that signature,
/// with its own images rather than the message's. The body keeps its
/// formatting: its section of the HTML, or all of a draft written elsewhere
/// that has neither a signature nor a forward here.
fn stored_parts(store: &Store, text: &str, html: Option<&str>) -> Result<StoredParts> {
    // A reply saved in another app quotes the original, tracking pixels and all.
    let html = html.map(trackers::strip);
    let html = html.as_deref();
    // What the HTML version adds after a body (`Parts::html`) isn't typed.
    let body_html = html
        .and_then(|h| signature::section(h, BODY_SECTION))
        .map(|body| {
            let body = body.trim();
            body.strip_suffix("<br><br>")
                .unwrap_or(body)
                .trim_end()
                .to_owned()
        });
    let whole = || match text.find(FORWARDED) {
        Some(at) if at == 0 || text[..at].ends_with('\n') => StoredParts {
            body: text[..at].trim_end().to_owned(),
            quote: Some(text[at..].trim_end().to_owned()),
            ..Default::default()
        },
        _ => StoredParts {
            body: text.trim_end().to_owned(),
            ..Default::default()
        },
    };
    let Some(found) = html.and_then(|h| signature::section(h, SIGNATURE_SECTION)) else {
        let mut parts = whole();
        parts.body_html = match body_html {
            Some(body) => Some(body),
            None if parts.quote.is_none() => html
                .map(signature::sanitize)
                .filter(|h| !signature::text(h).is_empty() || h.contains("<img")),
            None => None,
        };
        return Ok(parts);
    };
    let found_text = signature::text(found);
    let squeeze = |t: &str| t.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let saved = store
        .signatures()?
        .into_iter()
        .find(|s| !found_text.is_empty() && squeeze(&s.text) == squeeze(&found_text));
    let (body, quote) = match signature::find_text(text, &found_text) {
        Some((start, end)) => (text[..start].trim_end(), text[end..].trim()),
        // Only images: nothing to find in the text.
        None if found_text.is_empty() => (text.trim_end(), ""),
        // Not where it should be: leave the text as it is.
        None => return Ok(whole()),
    };
    Ok(StoredParts {
        body: body.to_owned(),
        body_html,
        signature_id: saved.as_ref().map(|s| s.id),
        signature: Some(saved.map_or_else(|| found.trim().to_owned(), |s| s.html)),
        quote: (!quote.is_empty()).then(|| quote.to_owned()),
    })
}

/// Stored display strings as an address field, quoting names with commas.
fn field(list: &[String]) -> String {
    list.iter()
        .map(|entry| match entry.rsplit_once(" <") {
            Some((name, address)) if name.contains([',', ';']) && !name.starts_with('"') => {
                format!("\"{}\" <{address}", name.replace('"', ""))
            }
            _ => entry.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The addresses in a field that parse; a draft may hold half-typed ones.
pub fn recipients(field: &str) -> Vec<Mailbox> {
    let field = field.replace(';', ",");
    draft::split_addresses(&field)
        .into_iter()
        .filter_map(|part| draft::recipients(part).ok())
        .flatten()
        .collect()
}

/// Everyone a draft goes to, as the store notes them for recipient suggestions.
pub fn sent_to(draft: &Draft) -> Vec<Contact> {
    [&draft.to, &draft.cc, &draft.bcc]
        .into_iter()
        .flat_map(|field| recipients(field))
        .map(|m| Contact {
            name: m.name.unwrap_or_default(),
            address: m.email.to_string(),
        })
        .collect()
}

/// An address field as the store keeps it: `Name <address>` or the bare
/// address, and what doesn't parse as typed.
pub fn display_list(field: &str) -> Vec<String> {
    let field = field.replace(';', ",");
    draft::split_addresses(&field)
        .into_iter()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .flat_map(|part| match draft::recipients(part) {
            Ok(list) => list
                .into_iter()
                .map(|m| match m.name.filter(|n| !n.trim().is_empty()) {
                    Some(name) => format!("{name} <{}>", m.email),
                    None => m.email.to_string(),
                })
                .collect(),
            Err(_) => vec![part.to_owned()],
        })
        .collect()
}

/// A reply's threading: the original's Message-ID, and the References to
/// send (the original's, then its own id). From the stored original, else
/// the draft's own headers.
pub fn threading(store: &Store, draft: &Draft) -> Result<Option<(String, String)>> {
    let (id, references) = match draft.reply_to {
        Some(original) => match store.headers(original)? {
            Some(headers) if headers.message_id.is_some() => {
                (headers.message_id.unwrap_or_default(), headers.references)
            }
            _ => return Ok(None),
        },
        None => match &draft.in_reply_to {
            Some(id) => (id.clone(), draft.references.clone()),
            None => return Ok(None),
        },
    };
    let references = references
        .unwrap_or_default()
        .split_whitespace()
        .filter(|r| *r != id)
        .chain([id.as_str()])
        .collect::<Vec<_>>()
        .join(" ");
    Ok(Some((id, references)))
}

/// The original of a reply that only has the draft's headers: it threads,
/// with nothing to quote.
pub fn original_from_headers(draft: &Draft) -> Option<(Original, Option<String>)> {
    let message_id = draft.in_reply_to.clone()?;
    Some((
        Original {
            message_id: Some(message_id),
            references: draft.references.clone(),
            ..Default::default()
        },
        None,
    ))
}

/// A Message-ID on the sender's domain, without angle brackets. Each upload
/// gets its own, so an old copy is never mistaken for the new one.
pub fn new_message_id(domain: &str) -> String {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).expect("the OS provides randomness");
    let id: String = random.iter().map(|b| format!("{b:02x}")).collect();
    format!("{id}@{domain}")
}

/// The draft as a message for the Drafts folder: like a sent one, but with
/// Bcc kept, recipients optional, and no reply quote (the original is
/// quoted when it's sent). A forward's original is kept below the signature.
pub fn mime(
    from: Mailbox,
    draft: &Draft,
    message_id: &str,
    threading: Option<&(String, String)>,
    files: &[AttachmentData],
) -> Result<Vec<u8>> {
    let invalid =
        |e: &dyn std::fmt::Display| Error::Config(format!("Couldn't build the draft: {e}"));
    // Nothing is delivered, but lettre wants an envelope.
    let envelope = Envelope::new(Some(from.email.clone()), vec![from.email.clone()])
        .map_err(|e| invalid(&e))?;
    let mut builder = lettre::Message::builder()
        .message_id(Some(format!("<{message_id}>")))
        .from(from)
        .subject(draft.subject.trim())
        .envelope(envelope)
        .keep_bcc();
    for m in recipients(&draft.to) {
        builder = builder.to(m);
    }
    for m in recipients(&draft.cc) {
        builder = builder.cc(m);
    }
    for m in recipients(&draft.bcc) {
        builder = builder.bcc(m);
    }
    if let Some((id, references)) = threading {
        builder = builder.in_reply_to(format!("<{id}>"));
        let references: Vec<String> = references
            .split_whitespace()
            .map(|r| format!("<{r}>"))
            .collect();
        builder = builder.raw_header(HeaderValue::new(
            HeaderName::new_from_ascii_str("References"),
            references.join(" "),
        ));
    }
    let built = draft.draft_bodies(None).build(builder, files);
    Ok(built.map_err(|e| invalid(&e))?.formatted())
}

/// Uploads the account's drafts saved before `settled_before` to its Drafts
/// folder, then deletes the copies they replaced and those discarded or
/// sent. Network and sign-in failures fail the round (everything stays
/// queued); a draft the server refuses stays on this computer.
pub async fn push_imap(
    session: &mut Session,
    store: &Mutex<Store>,
    account: &Account,
    state: &SyncState,
    settled_before: i64,
) -> Result<DraftsReport> {
    let mut report = DraftsReport::default();
    let uploads = lock(store).draft_uploads(account.id, settled_before)?;
    if !uploads.is_empty() {
        let (mailbox, listed) = drafts_mailbox(session, store, account.id, state).await?;
        report.folders_changed = listed;
        let from = from_mailbox(account)?;
        for upload in uploads {
            match upload_imap(session, store, state, &from, &mailbox, &upload).await? {
                None => report.uploaded += 1,
                Some(refused) => {
                    lock(store).drop_draft_upload(upload.message_id)?;
                    report.errors.push(refused);
                }
            }
        }
    }
    let removals = lock(store).draft_removals(account.id)?;
    if !removals.is_empty() {
        let uidplus = session.capabilities().await?.has_str("UIDPLUS");
        for removal in removals {
            remove_imap(session, &removal.copy, uidplus).await?;
            lock(store).draft_removed(removal.id)?;
            report.removed += 1;
        }
    }
    Ok(report)
}

/// The account's Drafts mailbox: the stored folder, else one the server
/// marks or names as drafts, else a new "Drafts". True when the folders
/// were listed again for it.
async fn drafts_mailbox(
    session: &mut Session,
    store: &Mutex<Store>,
    account_id: i64,
    state: &SyncState,
) -> Result<(String, bool)> {
    let stored = lock(store)
        .folders()?
        .into_iter()
        .find(|f| f.account_id == account_id && f.role == Some(FolderRole::Drafts));
    if let Some(folder) = stored {
        return Ok((folder.remote_id, false));
    }
    let names: Vec<Name> = session
        .list(Some(""), Some("*"))
        .await?
        .try_collect()
        .await?;
    let leaf = |n: &Name| {
        n.delimiter()
            .and_then(|d| n.name().rsplit_once(d))
            .map_or(n.name(), |(_, leaf)| leaf)
            .to_owned()
    };
    let found = names
        .iter()
        .find(|n| n.attributes().contains(&NameAttribute::Drafts))
        .or_else(|| {
            names.iter().find(|n| {
                ["Drafts", "Draft"]
                    .iter()
                    .any(|d| leaf(n).eq_ignore_ascii_case(d))
            })
        })
        .map(|n| n.name().to_owned());
    let mailbox = match found {
        Some(mailbox) => mailbox,
        None => imap::create_folder(session, None, "Drafts").await?,
    };
    // Listed now, so the uploaded draft is filed in it.
    let listed = imap::list_folders(session, state).await?;
    lock(store).replace_folders(account_id, &listed)?;
    Ok((mailbox, true))
}

/// Appends one draft as `\Draft \Seen` and records where it is. `Some`
/// with the reason if the server or the draft itself is refused.
async fn upload_imap(
    session: &mut Session,
    store: &Mutex<Store>,
    state: &SyncState,
    from: &Mailbox,
    mailbox: &str,
    upload: &DraftUpload,
) -> Result<Option<String>> {
    let Ok(draft) = serde_json::from_str::<Draft>(&upload.draft) else {
        return Ok(Some("A saved draft is unreadable.".into()));
    };
    let mut files = Vec::with_capacity(draft.attachments.len());
    let mut saved = lock(store).draft_files(upload.message_id)?.into_iter();
    for a in &draft.attachments {
        match a {
            OutgoingAttachment::File { .. } => files.extend(saved.next().map(|f| AttachmentData {
                name: f.name,
                mime: f.mime,
                bytes: f.data,
            })),
            OutgoingAttachment::Message {
                message,
                index,
                name,
                ..
            } => {
                let Some((at, index, _)) = source(store, *message, *index, name)? else {
                    continue;
                };
                // A source that's gone (or a mailbox renamed away) loses its file, not the draft.
                match imap::fetch_raw(session, state, &at).await {
                    Ok(Some(raw)) => files.extend(attachment::extract(&raw, index, name)),
                    Ok(None) | Err(Error::Server(_)) => {}
                    Err(e) => return Err(e),
                }
            }
        }
    }
    let threading = threading(&lock(store), &draft)?;
    let message_id = new_message_id(from.email.domain());
    let raw = match mime(
        from.clone(),
        &draft,
        &message_id,
        threading.as_ref(),
        &files,
    ) {
        Ok(raw) => raw,
        Err(e) => return Ok(Some(e.to_string())),
    };
    match session
        .append(mailbox, Some("(\\Draft \\Seen)"), None, &raw)
        .await
    {
        Ok(()) => {}
        Err(async_imap::error::Error::No(text) | async_imap::error::Error::Bad(text)) => {
            return Ok(Some(format!("The mail server refused a draft: {text}")));
        }
        Err(e) => return Err(e.into()),
    }
    // APPEND doesn't say the new UID without UIDPLUS; the new Message-ID finds it.
    session.select(mailbox).await?;
    let uid = session
        .uid_search(format!("HEADER Message-ID {}", imap::quoted(&message_id)))
        .await?
        .into_iter()
        .max();
    let copy = DraftCopy {
        mailbox: Some(mailbox.to_owned()),
        uid,
        remote_id: None,
        message_id: Some(message_id),
    };
    let store = &mut lock(store);
    store.draft_uploaded(upload, &copy, &described(&files))?;
    // An auto-draft's own copy: another app replacing it moves it to a new UID.
    let at = uid.map(|uid| imap_version(mailbox, uid));
    store.auto_draft_uploaded(upload.message_id, at.as_deref())?;
    Ok(None)
}

/// An IMAP draft copy's version as `Store::auto_draft_uploaded` keeps it:
/// where it is. IMAP messages don't change; an edit is a new message.
pub(crate) fn imap_version(mailbox: &str, uid: u32) -> String {
    format!("{uid} {mailbox}")
}

/// The UID in an `imap_version` of a copy in `mailbox`.
pub(crate) fn imap_uid(version: &str, mailbox: &str) -> Option<u32> {
    let (uid, at) = version.split_once(' ')?;
    (at == mailbox).then(|| uid.parse().ok()).flatten()
}

/// Deletes a draft's server copy by its verified Message-ID. A UID without
/// generation metadata is insufficient; an already missing copy is fine.
async fn remove_imap(session: &mut Session, copy: &DraftCopy, uidplus: bool) -> Result<()> {
    let Some(mailbox) = &copy.mailbox else {
        return Ok(());
    };
    match session.select(mailbox).await {
        Ok(_) => {}
        Err(async_imap::error::Error::No(_)) => return Ok(()),
        Err(e) => return Err(e.into()),
    }
    let uids: Vec<u32> = match &copy.message_id {
        Some(id) => {
            let found = imap::find_message_id(session, id).await?;
            match copy.uid {
                Some(uid) if found.contains(&uid) => vec![uid],
                _ => found,
            }
        }
        None if copy.uid.is_some() => {
            return Err(Error::Server(
                "That saved draft has no Message-ID, so it cannot be removed safely.".into(),
            ));
        }
        None => Vec::new(),
    };
    if uids.is_empty() {
        return Ok(());
    }
    if !uidplus {
        return Err(Error::Server(
            "This mail server needs UIDPLUS to remove saved drafts safely.".into(),
        ));
    }
    let set = imap::uid_set(&uids);
    let result: async_imap::error::Result<()> = async {
        let _: Vec<Fetch> = session
            .uid_store(&set, "+FLAGS.SILENT (\\Deleted)")
            .await?
            .try_collect()
            .await?;
        let _: Vec<_> = session.uid_expunge(&set).await?.try_collect().await?;
        Ok(())
    }
    .await;
    match result {
        Ok(()) | Err(async_imap::error::Error::No(_)) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Graph's `push_imap`: drafts are updated in place when no files are
/// involved, else made anew (the old copy is deleted).
pub async fn push_graph(
    graph: &Graph,
    store: &Mutex<Store>,
    account: &Account,
    settled_before: i64,
) -> Result<DraftsReport> {
    let mut report = DraftsReport::default();
    let uploads = lock(store).draft_uploads(account.id, settled_before)?;
    for upload in uploads {
        let Ok(draft) = serde_json::from_str::<Draft>(&upload.draft) else {
            lock(store).drop_draft_upload(upload.message_id)?;
            report.errors.push("A saved draft is unreadable.".into());
            continue;
        };
        let mut files = Vec::with_capacity(draft.attachments.len());
        let mut saved = lock(store).draft_files(upload.message_id)?.into_iter();
        for a in &draft.attachments {
            match a {
                OutgoingAttachment::File { .. } => {
                    files.extend(saved.next().map(|f| AttachmentData {
                        name: f.name,
                        mime: f.mime,
                        bytes: f.data,
                    }))
                }
                OutgoingAttachment::Message {
                    message,
                    index,
                    name,
                    ..
                } => {
                    let Some((at, index, wanted)) = source(store, *message, *index, name)? else {
                        continue;
                    };
                    let Some(remote) = at.remote_id else { continue };
                    match graph.attachment(&remote, index, &wanted).await {
                        Ok(f) => files.push(f),
                        Err(Error::Gone | Error::Server(_)) => {}
                        Err(e) => return Err(e),
                    }
                }
            }
        }
        let update = upload
            .copy
            .as_ref()
            .and_then(|c| c.remote_id.as_deref())
            .filter(|_| files.is_empty() && upload.attachments.is_empty());
        match graph.save_draft(update, &draft, &files).await {
            Ok(saved) => {
                let copy = DraftCopy {
                    remote_id: Some(saved.id),
                    message_id: saved.message_id,
                    ..Default::default()
                };
                let store = &mut lock(store);
                store.draft_uploaded(&upload, &copy, &described(&files))?;
                // An auto-draft's own copy, to tell it from one Outlook changed.
                store.auto_draft_uploaded(upload.message_id, saved.version.as_deref())?;
                report.uploaded += 1;
            }
            Err(e @ (Error::Network { .. } | Error::Auth(_))) => return Err(e),
            Err(e) => {
                lock(store).drop_draft_upload(upload.message_id)?;
                report.errors.push(e.to_string());
            }
        }
    }
    let removals = lock(store).draft_removals(account.id)?;
    for removal in removals {
        if let Some(id) = &removal.copy.remote_id {
            match graph.delete_draft(id).await {
                Ok(()) => {}
                Err(e @ (Error::Network { .. } | Error::Auth(_))) => return Err(e),
                Err(e) => report.errors.push(e.to_string()),
            }
        }
        lock(store).draft_removed(removal.id)?;
        report.removed += 1;
    }
    Ok(report)
}

/// Where attachment `index` of a stored message is, as listed there. A list
/// that changed since (the draft was uploaded again) is searched by name.
fn source(
    store: &Mutex<Store>,
    message: i64,
    index: usize,
    name: &str,
) -> Result<Option<(Location, usize, Attachment)>> {
    let store = lock(store);
    let (Some(at), Some(content)) = (store.location(message)?, store.content(message)?) else {
        return Ok(None);
    };
    let index = match content.attachments.get(index) {
        Some(a) if a.name == name => index,
        _ => match content.attachments.iter().position(|a| a.name == name) {
            Some(i) => i,
            None => return Ok(None),
        },
    };
    let wanted = content.attachments[index].clone();
    Ok(Some((at, index, wanted)))
}

/// What the uploaded copy has attached, for the store.
fn described(files: &[AttachmentData]) -> Vec<Attachment> {
    files
        .iter()
        .map(|f| Attachment {
            name: f.name.clone(),
            mime: f.mime.clone(),
            size: f.bytes.len() as u64,
            remote_id: None,
        })
        .collect()
}

pub fn from_mailbox(account: &Account) -> Result<Mailbox> {
    Ok(Mailbox::new(
        Some(account.name.clone()).filter(|n| !n.is_empty()),
        account
            .email
            .parse()
            .map_err(|_| Error::Config("The account's address is invalid.".into()))?,
    ))
}

fn lock(store: &Mutex<Store>) -> MutexGuard<'_, Store> {
    store
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_auto_drafts_server_copy_keeps_its_version() {
        use crate::graph::tests::{Answer, serve};
        use spark_store::{AccountKind, Added, DraftOption, Incoming, NewAccount, RemoteFolder};
        let (base, _) = serve(
            |asked, _| match (asked.method.as_str(), asked.path.as_str()) {
                ("POST", "/me/messages") => Answer::json(serde_json::json!({
                    "id": "AAMkD",
                    "internetMessageId": "<d@contoso.com>",
                    "changeKey": "CQAAABYAAAD"
                })),
                _ => Answer::status(404),
            },
        )
        .await;
        let mut store = Store::open_in_memory().unwrap();
        let account = store
            .add_account(&NewAccount {
                kind: AccountKind::Microsoft,
                email: "me@contoso.com",
                name: "Me",
                label: "Work",
                config: "{}",
            })
            .unwrap();
        store
            .replace_folders(
                account.id,
                &[RemoteFolder {
                    remote_id: "drafts".into(),
                    parent: None,
                    name: "Drafts".into(),
                    role: Some(FolderRole::Drafts),
                    selectable: true,
                }],
            )
            .unwrap();
        let Added::New(original) = store
            .add_incoming(
                &Incoming {
                    account_id: account.id,
                    remote_id: Some("AAMkO".into()),
                    from_address: "dana@contoso.com".into(),
                    subject: "Lunch".into(),
                    ..Default::default()
                },
                false,
            )
            .unwrap()
        else {
            panic!("new");
        };
        let draft = Draft {
            account: account.id,
            to: "dana@contoso.com".into(),
            subject: "Re: Lunch".into(),
            body: "Sure.".into(),
            ..Default::default()
        };
        let id = save(&mut store, &account, &draft, &[], 1).unwrap();
        let option = [DraftOption {
            title: String::new(),
            body: "Sure.".into(),
        }];
        store.add_auto_draft(id, original, &option, 0, 1).unwrap();
        let store = Mutex::new(store);
        let graph = Graph::with_base(&base, "token");
        let report = push_graph(&graph, &store, &account, i64::MAX)
            .await
            .unwrap();
        assert_eq!(report.uploaded, 1);
        assert_eq!(
            lock(&store)
                .auto_draft_server_version(id)
                .unwrap()
                .as_deref(),
            Some("CQAAABYAAAD")
        );
    }

    #[test]
    fn send_later_survives_restart_and_draft_upload_without_queueing_delivery() {
        let path = std::env::temp_dir().join(format!(
            "spark-scheduled-draft-{}.sqlite",
            new_message_id("test")
        ));
        let mut store = Store::open(&path).unwrap();
        let account = store
            .add_account(&spark_store::NewAccount {
                kind: spark_store::AccountKind::Imap,
                email: "me@example.com",
                name: "Me",
                label: "Test",
                config: "{}",
            })
            .unwrap();
        store
            .replace_folders(
                account.id,
                &[spark_store::RemoteFolder {
                    remote_id: "Drafts".into(),
                    parent: None,
                    name: "Drafts".into(),
                    role: Some(FolderRole::Drafts),
                    selectable: true,
                }],
            )
            .unwrap();
        let schedule = serde_json::json!({"key": "tomorrow", "at": 2_000_000_000_000_i64});
        let mut json = serde_json::to_value(draft()).unwrap();
        json["sendLater"] = schedule.clone();
        json["sendAt"] = serde_json::json!(2_000_000_000_000_i64);
        let d: Draft = serde_json::from_value(json).unwrap();
        let id = save(&mut store, &account, &d, &[], 1).unwrap();
        drop(store);

        let mut store = Store::open(&path).unwrap();
        let opened = open(&store, id).unwrap().unwrap();
        assert_eq!(opened.send_at, 0, "saving must never queue delivery");
        assert_eq!(
            serde_json::to_value(&opened).unwrap()["sendLater"],
            schedule
        );
        assert!(
            store
                .claim_due_sends(account.id, i64::MAX)
                .unwrap()
                .is_empty()
        );
        let upload = store.draft_uploads(account.id, i64::MAX).unwrap().remove(0);
        store
            .draft_uploaded(
                &upload,
                &DraftCopy {
                    mailbox: Some("Drafts".into()),
                    uid: Some(10),
                    message_id: Some("scheduled@example.com".into()),
                    remote_id: None,
                },
                &[],
            )
            .unwrap();
        assert!(
            store.draft(id).unwrap().is_none(),
            "upload removes the pending JSON"
        );
        drop(store);

        let mut store = Store::open(&path).unwrap();
        let opened = open(&store, id).unwrap().unwrap();
        assert_eq!(
            serde_json::to_value(&opened).unwrap()["sendLater"],
            schedule
        );
        let mut cleared = serde_json::to_value(opened).unwrap();
        cleared["sendLater"] = serde_json::Value::Null;
        let cleared: Draft = serde_json::from_value(cleared).unwrap();
        save(&mut store, &account, &cleared, &[], 2).unwrap();
        let upload = store.draft_uploads(account.id, i64::MAX).unwrap().remove(0);
        store
            .draft_uploaded(
                &upload,
                &DraftCopy {
                    mailbox: Some("Drafts".into()),
                    uid: Some(11),
                    message_id: Some("resaved@example.com".into()),
                    remote_id: None,
                },
                &[],
            )
            .unwrap();
        assert!(
            serde_json::to_value(open(&store, id).unwrap().unwrap()).unwrap()["sendLater"]
                .is_null()
        );
        assert!(
            store
                .claim_due_sends(account.id, i64::MAX)
                .unwrap()
                .is_empty()
        );
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    fn draft() -> Draft {
        Draft {
            account: 1,
            to: "Ana Ruiz <ana@x.com>, bob@".into(),
            cc: String::new(),
            bcc: "boss@x.com".into(),
            subject: " Plan ".into(),
            body: "Half done".into(),
            reply_to: None,
            send_at: 0,
            attachments: Vec::new(),
            draft_id: None,
            in_reply_to: Some("1@x.com".into()),
            references: Some("0@x.com 1@x.com".into()),
            ..Default::default()
        }
    }

    #[test]
    fn half_typed_addresses_are_kept_for_the_list_and_left_out_of_the_message() {
        assert_eq!(
            display_list("Ana Ruiz <ana@x.com>; bob@, \"Doe, John\" <john@z.net>"),
            ["Ana Ruiz <ana@x.com>", "bob@", "Doe, John <john@z.net>"]
        );
        let emails: Vec<String> = recipients("Ana <ana@x.com>, bob@, carl@y.org")
            .iter()
            .map(|m| m.email.to_string())
            .collect();
        assert_eq!(emails, ["ana@x.com", "carl@y.org"]);
    }

    #[test]
    fn drafts_keep_bcc_threading_and_files() {
        let store = Store::open_in_memory().unwrap();
        let threading = threading(&store, &draft()).unwrap();
        assert_eq!(
            threading,
            Some(("1@x.com".to_owned(), "0@x.com 1@x.com".to_owned()))
        );
        let from: Mailbox = "Me <me@x.com>".parse().unwrap();
        let files = [AttachmentData {
            name: "plan.pdf".into(),
            mime: "application/pdf".into(),
            bytes: b"%PDF".to_vec(),
        }];
        let raw = mime(from, &draft(), "d1@x.com", threading.as_ref(), &files).unwrap();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("Message-ID: <d1@x.com>\r\n"), "{text}");
        assert!(text.contains("Bcc: boss@x.com\r\n"), "{text}");
        assert!(text.contains("In-Reply-To: <1@x.com>\r\n"), "{text}");
        assert!(
            text.contains("References: <0@x.com> <1@x.com>\r\n"),
            "{text}"
        );
        let parsed = crate::parse::parse_message(&raw).unwrap();
        assert_eq!(parsed.to, ["Ana Ruiz <ana@x.com>"]);
        assert_eq!(parsed.bcc, ["boss@x.com"]);
        assert_eq!(parsed.subject, "Plan");
        assert_eq!(parsed.text.trim(), "Half done");
        assert_eq!(parsed.attachments[0].name, "plan.pdf");

        // Nobody to send to yet is fine for a draft.
        let empty = Draft {
            to: String::new(),
            bcc: String::new(),
            in_reply_to: None,
            ..draft()
        };
        let from: Mailbox = "me@x.com".parse().unwrap();
        let raw = mime(from, &empty, "d2@x.com", None, &[]).unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("\r\nTo:"));
    }

    #[test]
    fn drafts_open_as_saved_then_from_their_server_copy() {
        let mut store = Store::open_in_memory().unwrap();
        let account = store
            .add_account(&spark_store::NewAccount {
                kind: spark_store::AccountKind::Imap,
                email: "me@x.com",
                name: "",
                label: "x",
                config: "{}",
            })
            .unwrap();
        store
            .replace_folders(
                account.id,
                &[spark_store::RemoteFolder {
                    remote_id: "Drafts".into(),
                    parent: None,
                    name: "Drafts".into(),
                    role: Some(FolderRole::Drafts),
                    selectable: true,
                }],
            )
            .unwrap();
        let mut d = Draft {
            to: "\"Doe, John\" <john@z.net>, bob@".into(),
            attachments: vec![OutgoingAttachment::File {
                path: "/tmp/plan.pdf".into(),
                name: "plan.pdf".into(),
                size: 4,
            }],
            ..draft()
        };
        d.account = account.id;
        let id = save(&mut store, &account, &d, &[], 1_000).unwrap();
        let m = store.message(id).unwrap().unwrap();
        assert_eq!(
            (m.from_name.as_str(), m.subject.as_str()),
            ("me@x.com", "Plan")
        );
        assert_eq!(m.to, ["Doe, John <john@z.net>", "bob@"]);
        assert_eq!(store.in_reply_to(id).unwrap().as_deref(), Some("1@x.com"));
        // Waiting to go up: exactly as saved.
        let opened = open(&store, id).unwrap().unwrap();
        assert_eq!(
            opened,
            Draft {
                draft_id: Some(id),
                ..d.clone()
            }
        );

        // On the server: from the stored copy, its file kept by reference.
        let upload = store.draft_uploads(account.id, i64::MAX).unwrap().remove(0);
        let copy = DraftCopy {
            mailbox: Some("Drafts".into()),
            uid: Some(9),
            remote_id: None,
            message_id: Some("d9@x.com".into()),
        };
        let files = [Attachment {
            name: "plan.pdf".into(),
            mime: "application/pdf".into(),
            size: 4,
            remote_id: None,
        }];
        store.draft_uploaded(&upload, &copy, &files).unwrap();
        let opened = open(&store, id).unwrap().unwrap();
        assert_eq!(opened.to, "\"Doe, John\" <john@z.net>, bob@");
        assert_eq!(opened.bcc, "boss@x.com");
        assert_eq!(
            (opened.subject.as_str(), opened.body.as_str()),
            ("Plan", "Half done")
        );
        assert_eq!(opened.draft_id, Some(id));
        // Its original isn't stored: it threads by the headers.
        assert_eq!(opened.reply_to, None);
        assert_eq!(opened.in_reply_to.as_deref(), Some("1@x.com"));
        assert_eq!(opened.references.as_deref(), Some("0@x.com"));
        assert_eq!(
            opened.attachments,
            [OutgoingAttachment::Message {
                message: id,
                index: 0,
                name: "plan.pdf".into(),
                size: 4
            }]
        );
        assert!(open(&store, id + 1).unwrap().is_none());
    }

    #[test]
    fn rich_text_comes_back_from_the_stored_copy() {
        let store = Store::open_in_memory().unwrap();
        let rich = Draft {
            account: 1,
            to: "bo@x.com".into(),
            subject: "Plan".into(),
            body: "Hi Bo".into(),
            body_html: Some("<div>Hi <b>Bo</b></div>".into()),
            ..Default::default()
        };
        let from: Mailbox = "me@x.com".parse().unwrap();
        let raw = mime(from.clone(), &rich, "d1@x.com", None, &[]).unwrap();
        let parsed = mail_parser::MessageParser::default().parse(&raw).unwrap();
        let text = parsed.body_text(0).unwrap().replace("\r\n", "\n");
        let parts = stored_parts(&store, &text, parsed.body_html(0).as_deref()).unwrap();
        assert_eq!(
            (parts.body.as_str(), parts.body_html.as_deref()),
            ("Hi Bo", Some("<div>Hi <b>Bo</b></div>"))
        );
        // A template's placeholders stay for the composer to show.
        let template = Draft {
            body_html: Some(
                r#"<div>Hi <span data-placeholder="custom" data-label="Company">Company</span></div>"#
                    .into(),
            ),
            ..rich.clone()
        };
        let raw = mime(from.clone(), &template, "d3@x.com", None, &[]).unwrap();
        let parsed = mail_parser::MessageParser::default().parse(&raw).unwrap();
        let text = parsed.body_text(0).unwrap().replace("\r\n", "\n");
        let parts = stored_parts(&store, &text, parsed.body_html(0).as_deref()).unwrap();
        assert_eq!(
            parts.body_html.as_deref(),
            Some(
                r#"<div>Hi <span data-placeholder="custom" data-label="Company">Company</span></div>"#
            )
        );
        assert!(
            !template
                .bodies(None, None)
                .html
                .unwrap()
                .contains("data-placeholder"),
            "sent mail goes without them"
        );
        // With a signature, the body is still its own section.
        let signed = Draft {
            signature: Some("<div>Me</div>".into()),
            ..rich.clone()
        };
        let raw = mime(from, &signed, "d2@x.com", None, &[]).unwrap();
        let parsed = mail_parser::MessageParser::default().parse(&raw).unwrap();
        let text = parsed.body_text(0).unwrap().replace("\r\n", "\n");
        let parts = stored_parts(&store, &text, parsed.body_html(0).as_deref()).unwrap();
        assert_eq!(parts.body_html.as_deref(), Some("<div>Hi <b>Bo</b></div>"));
        assert_eq!(parts.signature.as_deref(), Some("<div>Me</div>"));
        // One written elsewhere keeps all its formatting, cleaned.
        let elsewhere = stored_parts(
            &store,
            "Hello",
            Some(r#"<html><head><style>p{}</style></head><body><p onclick="x()"><i>Hello</i></p></body></html>"#),
        )
        .unwrap();
        assert_eq!(elsewhere.body_html.as_deref(), Some("<p><i>Hello</i></p>"));
        // A reply saved in Outlook or Gmail quotes the original's tracking pixel; it doesn't load here.
        let reply = stored_parts(
            &store,
            "Sounds good",
            Some(r#"<div>Sounds good</div><div id="divRplyFwdMsg">From: Shop</div><p>Sale</p><img src="https://t.example.com/open.gif" width="1" height="1">"#),
        )
        .unwrap();
        let html = reply.body_html.unwrap();
        assert!(
            html.contains("<p>Sale</p>") && !html.contains("open.gif"),
            "{html}"
        );
    }

    #[test]
    fn signatures_and_forwards_come_back_from_the_stored_copy() {
        let mut store = Store::open_in_memory().unwrap();
        let account = store
            .add_account(&spark_store::NewAccount {
                kind: spark_store::AccountKind::Imap,
                email: "me@x.com",
                name: "",
                label: "x",
                config: "{}",
            })
            .unwrap();
        store
            .replace_folders(
                account.id,
                &[spark_store::RemoteFolder {
                    remote_id: "Drafts".into(),
                    parent: None,
                    name: "Drafts".into(),
                    role: Some(FolderRole::Drafts),
                    selectable: true,
                }],
            )
            .unwrap();
        let saved = store
            .add_signature(
                r#"<div>Me</div><img src="data:image/png;base64,iVBORw0KGgo=">"#,
                "Me",
                1,
            )
            .unwrap();
        let forward = Draft {
            account: account.id,
            to: "bo@x.com".into(),
            subject: "Fwd: Plan".into(),
            body: "FYI".into(),
            signature: Some(saved.html.clone()),
            signature_id: Some(saved.id),
            quote: Some("---------- Forwarded message ----------\nFrom: Bo\n\nThe plan".into()),
            ..Default::default()
        };
        let id = save(&mut store, &account, &forward, &[], 1_000).unwrap();
        let upload = store.draft_uploads(account.id, i64::MAX).unwrap().remove(0);
        store
            .draft_uploaded(
                &upload,
                &DraftCopy {
                    uid: Some(3),
                    mailbox: Some("Drafts".into()),
                    ..Default::default()
                },
                &[],
            )
            .unwrap();
        // From the stored copy: the saved signature, with its own image.
        let opened = open(&store, id).unwrap().unwrap();
        assert_eq!(opened.body, "FYI");
        assert_eq!(
            (opened.signature.as_deref(), opened.signature_id),
            (Some(saved.html.as_str()), Some(saved.id))
        );
        assert_eq!(opened.quote, forward.quote);

        // A server copy's parts, read from the MIME the server keeps, with a
        // signature that isn't saved here.
        let other = Draft {
            signature: Some("<div><b>Someone</b></div><div>Else</div>".into()),
            signature_id: None,
            ..forward.clone()
        };
        let from: Mailbox = "me@x.com".parse().unwrap();
        let raw = mime(from, &other, "d1@x.com", None, &[]).unwrap();
        let parsed = mail_parser::MessageParser::default().parse(&raw).unwrap();
        let text = parsed.body_text(0).unwrap().replace("\r\n", "\n");
        let parts = stored_parts(&store, &text, parsed.body_html(0).as_deref()).unwrap();
        assert_eq!(parts.body, "FYI");
        assert_eq!(
            parts.signature.as_deref(),
            Some("<div><b>Someone</b></div><div>Else</div>")
        );
        assert_eq!(parts.signature_id, None);
        assert_eq!(parts.quote, forward.quote);

        // Without a signature, a forward still splits at its header.
        let plain = stored_parts(
            &store,
            "FYI\n\n---------- Forwarded message ----------\nFrom: Bo",
            None,
        )
        .unwrap();
        assert_eq!(
            (plain.body.as_str(), plain.quote.as_deref()),
            (
                "FYI",
                Some("---------- Forwarded message ----------\nFrom: Bo")
            )
        );
        let typed = stored_parts(&store, "Just text", None).unwrap();
        assert_eq!((typed.body.as_str(), typed.quote), ("Just text", None));
    }

    #[test]
    fn replies_to_stored_mail_thread_by_the_original() {
        let mut store = Store::open_in_memory().unwrap();
        let account = store
            .add_account(&spark_store::NewAccount {
                kind: spark_store::AccountKind::Imap,
                email: "me@x.com",
                name: "Me",
                label: "x",
                config: "{}",
            })
            .unwrap();
        let original = match store
            .add_incoming(
                &spark_store::Incoming {
                    account_id: account.id,
                    mailbox: Some("INBOX".into()),
                    uid: Some(1),
                    message_id: Some("5@x.com".into()),
                    references: Some("4@x.com".into()),
                    from_address: "ana@x.com".into(),
                    ..Default::default()
                },
                false,
            )
            .unwrap()
        {
            spark_store::Added::New(id) => id,
            other => panic!("{other:?}"),
        };
        let reply = Draft {
            reply_to: Some(original),
            in_reply_to: None,
            references: None,
            ..draft()
        };
        assert_eq!(
            threading(&store, &reply).unwrap(),
            Some(("5@x.com".to_owned(), "4@x.com 5@x.com".to_owned()))
        );
        assert!(original_from_headers(&reply).is_none());
        let (headers_only, remote) = original_from_headers(&draft()).unwrap();
        assert_eq!(headers_only.message_id.as_deref(), Some("1@x.com"));
        assert!(headers_only.text.is_empty() && remote.is_none());
        assert!(new_message_id("x.com").ends_with("@x.com"));
        assert_ne!(new_message_id("x.com"), new_message_id("x.com"));
    }
}
