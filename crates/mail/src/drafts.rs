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
use lettre::message::header::{ContentType, HeaderName, HeaderValue};
use lettre::message::{Attachment as MimeAttachment, Mailbox, MultiPart, SinglePart};
use spark_store::{
    Account, Attachment, DraftCopy, DraftSave, DraftUpload, FolderRole, Location, OutboxFile, Store,
};

use crate::attachment::{self, AttachmentData, mime_for};
use crate::draft::{self, Draft, Original, OutgoingAttachment};
use crate::engine::Engine;
use crate::graph::Graph;
use crate::imap::{self, Session, SyncState};
use crate::{Error, Result, parse};

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
    /// Saves the composer's draft here, with copies of its files from this
    /// computer in order; the account's next rounds put it on the server.
    /// Returns the draft's message id.
    pub fn save_draft(&self, draft: &Draft, files: &[OutboxFile]) -> Result<i64> {
        let mut store = self.store();
        let account = store
            .account(draft.account)?
            .ok_or_else(|| Error::Config("That account was removed.".into()))?;
        save(&mut store, &account, draft, files, spark_store::now_ms())
    }

    /// A saved draft for the composer; `None` if `id` isn't a draft.
    pub fn open_draft(&self, id: i64) -> Result<Option<Draft>> {
        open(&self.store(), id)
    }
}

/// Saves the composer's draft as of `now` (see `Store::save_draft`).
pub fn save(
    store: &mut Store,
    account: &Account,
    draft: &Draft,
    files: &[OutboxFile],
    now: i64,
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
    let from_name = if account.name.is_empty() {
        &account.email
    } else {
        &account.name
    };
    Ok(store.save_draft(
        draft.draft_id,
        &DraftSave {
            account_id: account.id,
            draft: &json,
            from_name,
            from_address: &account.email,
            to: &display_list(&draft.to),
            cc: &display_list(&draft.cc),
            bcc: &display_list(&draft.bcc),
            subject: draft.subject.trim(),
            snippet: &parse::snippet(&draft.body),
            text: &draft.body,
            in_reply_to: threading.as_ref().map(|(id, _)| id.as_str()),
            references: threading.as_ref().map(|(_, refs)| refs.as_str()),
            attachments: &attachments,
            files,
        },
        now,
    )?)
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
    Ok(Some(Draft {
        account: m.account_id,
        to: field(&content.to),
        cc: field(&content.cc),
        bcc: field(&content.bcc),
        subject: m.subject,
        body: text,
        reply_to,
        send_at: 0,
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
    }))
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
/// Bcc kept, recipients optional, and the body as typed (the original is
/// quoted when it's sent).
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
    let built = if files.is_empty() {
        builder.body(draft.body.clone())
    } else {
        let mut mixed = MultiPart::mixed().singlepart(SinglePart::plain(draft.body.clone()));
        for f in files {
            let content_type = ContentType::parse(&f.mime)
                .unwrap_or_else(|_| ContentType::parse("application/octet-stream").expect("valid"));
            mixed = mixed.singlepart(
                MimeAttachment::new(f.name.clone()).body(f.bytes.clone(), content_type),
            );
        }
        builder.multipart(mixed)
    };
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
    lock(store).draft_uploaded(upload, &copy, &described(&files))?;
    Ok(None)
}

/// Deletes a draft's server copy: by Message-ID where known (checked
/// against its UID), else by UID. Already gone is fine.
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
            let found = session
                .uid_search(format!("HEADER Message-ID {}", imap::quoted(id)))
                .await?;
            match copy.uid {
                Some(uid) if found.contains(&uid) => vec![uid],
                _ => found.into_iter().collect(),
            }
        }
        None => copy.uid.into_iter().collect(),
    };
    if uids.is_empty() {
        return Ok(());
    }
    let set = imap::uid_set(&uids);
    let result: async_imap::error::Result<()> = async {
        let _: Vec<Fetch> = session
            .uid_store(&set, "+FLAGS.SILENT (\\Deleted)")
            .await?
            .try_collect()
            .await?;
        if uidplus {
            let _: Vec<_> = session.uid_expunge(&set).await?.try_collect().await?;
        } else {
            let _: Vec<_> = session.expunge().await?.try_collect().await?;
        }
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
            Ok((remote_id, message_id)) => {
                let copy = DraftCopy {
                    remote_id: Some(remote_id),
                    message_id,
                    ..Default::default()
                };
                lock(store).draft_uploaded(&upload, &copy, &described(&files))?;
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
