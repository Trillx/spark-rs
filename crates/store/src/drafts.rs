//! Drafts the composer saves. A draft is a message row in the account's
//! Drafts folder, so it lists at once; what's saved waits in `draft_uploads`
//! until the sync engine puts it on the server, replacing the copy the draft
//! had there. Server copies that were replaced, discarded or sent wait in
//! `draft_removals` to be deleted. Both survive restarts and being offline.

use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    Attachment, Category, Incoming, MessageState, OutboxFile, Place, Result, Store,
    attachments_from_json, insert_message, json,
};

/// Version 5: the draft queue; Bcc of stored mail (drafts keep theirs); and
/// the draft a queued send came from, which isn't listed while it waits.
pub(crate) const SCHEMA_DRAFTS: &str = "
CREATE TABLE draft_uploads (
    message_id INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    draft TEXT NOT NULL,
    saved_at INTEGER NOT NULL
);

CREATE TABLE draft_files (
    id INTEGER PRIMARY KEY,
    message_id INTEGER NOT NULL REFERENCES draft_uploads(message_id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    name TEXT NOT NULL,
    mime TEXT NOT NULL,
    data BLOB NOT NULL
);
CREATE INDEX draft_files_message ON draft_files(message_id, position);

CREATE TABLE draft_removals (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    mailbox TEXT,
    uid INTEGER,
    remote_id TEXT,
    message_id TEXT
);

ALTER TABLE messages ADD COLUMN bcc_list TEXT NOT NULL DEFAULT '[]';
ALTER TABLE outbox ADD COLUMN draft_message INTEGER REFERENCES messages(id) ON DELETE SET NULL;
";

/// What the composer saved, for the draft's message row.
pub struct DraftSave<'a> {
    pub account_id: i64,
    /// The composer's draft, JSON owned by the caller.
    pub draft: &'a str,
    pub from_name: &'a str,
    pub from_address: &'a str,
    /// Display strings, e.g. `Ana Ruiz <ana@example.com>`.
    pub to: &'a [String],
    pub cc: &'a [String],
    pub bcc: &'a [String],
    pub subject: &'a str,
    pub snippet: &'a str,
    pub text: &'a str,
    /// A reply's threading: the original's Message-ID, and References.
    pub in_reply_to: Option<&'a str>,
    pub references: Option<&'a str>,
    /// What's attached, for the list.
    pub attachments: &'a [Attachment],
    /// Copies of the files from this computer, in order.
    pub files: &'a [OutboxFile],
}

/// A draft's copy on the server: its Graph id, or its IMAP mailbox and UID
/// (or, when the UID isn't known, its Message-ID there).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DraftCopy {
    pub mailbox: Option<String>,
    pub uid: Option<u32>,
    pub remote_id: Option<String>,
    pub message_id: Option<String>,
}

impl DraftCopy {
    fn exists(&self) -> bool {
        self.remote_id.is_some() || self.uid.is_some() || self.message_id.is_some()
    }

    fn same(&self, other: &DraftCopy) -> bool {
        match (&self.remote_id, &other.remote_id) {
            (Some(a), Some(b)) => a == b,
            _ => self.uid.is_some() && self.uid == other.uid && self.mailbox == other.mailbox,
        }
    }
}

/// A saved draft waiting to go to the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftUpload {
    /// The draft's message row.
    pub message_id: i64,
    pub account_id: i64,
    /// The composer's draft as JSON.
    pub draft: String,
    pub saved_at: i64,
    /// The server copy it replaces, if it has one.
    pub copy: Option<DraftCopy>,
    /// That copy's attachments: what the draft's own attachments refer to.
    pub attachments: Vec<Attachment>,
}

/// A server copy of a draft to delete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftRemoval {
    pub id: i64,
    pub copy: DraftCopy,
}

impl Store {
    /// Saves the composer's draft: updates `existing` (the draft it was
    /// opened from or saved as before) or makes a new row in the account's
    /// Drafts folder, and queues it for the server. A draft whose account
    /// changed starts over in the new one. Returns the draft's message id.
    pub fn save_draft(&mut self, existing: Option<i64>, save: &DraftSave, now: i64) -> Result<i64> {
        let tx = self.conn.transaction()?;
        let kept = match existing {
            Some(id) if is_draft_in(&tx, id)? => {
                let account_id: i64 =
                    tx.query_row("SELECT account_id FROM messages WHERE id = ?1", [id], |r| {
                        r.get(0)
                    })?;
                if account_id == save.account_id {
                    Some(id)
                } else {
                    discard_in(&tx, id)?;
                    None
                }
            }
            _ => None,
        };
        let id = match kept {
            Some(id) => {
                // Until it's uploaded, the old copy's attachments are what
                // the draft's own refer to.
                let attachments = match server_copy(&tx, id)? {
                    Some(_) => None,
                    None => Some(json(save.attachments)),
                };
                tx.execute(
                    "UPDATE messages SET from_name = ?2, from_address = ?3, to_list = ?4,
                        cc_list = ?5, bcc_list = ?6, subject = ?7, snippet = ?8, date = ?9,
                        text_body = ?10, html_body = NULL, in_reply_to = ?11, refs = ?12,
                        attachments = COALESCE(?13, attachments)
                     WHERE id = ?1",
                    params![
                        id,
                        save.from_name,
                        save.from_address,
                        json(save.to),
                        json(save.cc),
                        json(save.bcc),
                        save.subject,
                        save.snippet,
                        now,
                        save.text,
                        save.in_reply_to,
                        save.references,
                        attachments
                    ],
                )?;
                id
            }
            None => {
                let m = Incoming {
                    account_id: save.account_id,
                    in_reply_to: save.in_reply_to.map(str::to_owned),
                    references: save.references.map(str::to_owned),
                    from_name: save.from_name.to_owned(),
                    from_address: save.from_address.to_owned(),
                    to: save.to.to_vec(),
                    cc: save.cc.to_vec(),
                    bcc: save.bcc.to_vec(),
                    subject: save.subject.to_owned(),
                    snippet: save.snippet.to_owned(),
                    date: now,
                    text: save.text.to_owned(),
                    attachments: save.attachments.to_vec(),
                    category: Category::People,
                    ..Default::default()
                };
                insert_message(&tx, &m, MessageState::Filed, false, Place::Folder)?
            }
        };
        tx.execute(
            "INSERT OR IGNORE INTO folder_members (folder_id, message_id)
             SELECT id, ?2 FROM folders WHERE account_id = ?1 AND role = 'drafts'
             ORDER BY position LIMIT 1",
            params![save.account_id, id],
        )?;
        // Saves within one millisecond still order.
        tx.execute(
            "INSERT INTO draft_uploads (message_id, draft, saved_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (message_id) DO UPDATE SET draft = excluded.draft,
                saved_at = MAX(excluded.saved_at, draft_uploads.saved_at + 1)",
            params![id, save.draft, now],
        )?;
        // Autosaves mostly change the text: files that are the same stay.
        let same_files = {
            let mut stmt = tx.prepare(
                "SELECT name, mime, data FROM draft_files WHERE message_id = ?1 ORDER BY position",
            )?;
            let stored: Vec<OutboxFile> = stmt
                .query_map([id], |r| {
                    Ok(OutboxFile {
                        name: r.get(0)?,
                        mime: r.get(1)?,
                        data: r.get(2)?,
                    })
                })?
                .collect::<Result<_>>()?;
            stored == save.files
        };
        if same_files {
            tx.commit()?;
            return Ok(id);
        }
        tx.execute("DELETE FROM draft_files WHERE message_id = ?1", [id])?;
        for (position, f) in save.files.iter().enumerate() {
            tx.execute(
                "INSERT INTO draft_files (message_id, position, name, mime, data)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, position as i64, f.name, f.mime, f.data],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }

    /// Whether a stored message is a draft: in its account's Drafts folder,
    /// or saved here and not uploaded yet.
    pub fn is_draft(&self, id: i64) -> Result<bool> {
        is_draft_in(&self.conn, id)
    }

    /// The draft as last saved here, while it waits to be uploaded.
    pub fn draft(&self, id: i64) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT draft FROM draft_uploads WHERE message_id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()
    }

    /// The files saved with a draft, in order.
    pub fn draft_files(&self, id: i64) -> Result<Vec<OutboxFile>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, mime, data FROM draft_files WHERE message_id = ?1 ORDER BY position",
        )?;
        stmt.query_map([id], |r| {
            Ok(OutboxFile {
                name: r.get(0)?,
                mime: r.get(1)?,
                data: r.get(2)?,
            })
        })?
        .collect()
    }

    /// Deletes a draft here, and queues its server copy for deleting.
    /// False if `id` isn't a draft.
    pub fn discard_draft(&mut self, id: i64) -> Result<bool> {
        let tx = self.conn.transaction()?;
        if !is_draft_in(&tx, id)? {
            return Ok(false);
        }
        discard_in(&tx, id)?;
        tx.commit()?;
        Ok(true)
    }

    /// A queued send came from this draft: it isn't listed while it waits,
    /// and goes once it's sent (`discard_draft`).
    pub fn hold_draft_for_send(&self, outbox_id: i64, draft: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE outbox SET draft_message = ?2 WHERE id = ?1",
            params![outbox_id, draft],
        )?;
        Ok(())
    }

    /// The account's drafts saved before `settled_before` (so quick saves
    /// in a row go up once), oldest first. Drafts waiting to be sent aren't
    /// uploaded: the send deletes them.
    pub fn draft_uploads(&self, account_id: i64, settled_before: i64) -> Result<Vec<DraftUpload>> {
        let queued: Vec<(i64, String, i64, String)> = {
            let mut stmt = self.conn.prepare(
                "SELECT u.message_id, u.draft, u.saved_at, m.attachments
                 FROM draft_uploads u JOIN messages m ON m.id = u.message_id
                 WHERE m.account_id = ?1 AND u.saved_at <= ?2
                   AND u.message_id NOT IN
                       (SELECT draft_message FROM outbox WHERE draft_message IS NOT NULL)
                 ORDER BY u.saved_at",
            )?;
            stmt.query_map(params![account_id, settled_before], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<Result<_>>()?
        };
        queued
            .into_iter()
            .map(|(message_id, draft, saved_at, attachments)| {
                Ok(DraftUpload {
                    message_id,
                    account_id,
                    draft,
                    saved_at,
                    copy: server_copy(&self.conn, message_id)?,
                    attachments: attachments_from_json(&attachments),
                })
            })
            .collect()
    }

    /// Records that `upload` is on the server as `copy`, with these
    /// attachments. The copy it replaced is queued for deleting, as is the
    /// new one if the draft was discarded meanwhile. If it was saved again
    /// meanwhile, that save stays queued.
    pub fn draft_uploaded(
        &mut self,
        upload: &DraftUpload,
        copy: &DraftCopy,
        attachments: &[Attachment],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        let id = upload.message_id;
        let exists: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM messages WHERE id = ?1)",
            [id],
            |r| r.get(0),
        )?;
        if !exists {
            queue_removal(&tx, upload.account_id, copy)?;
            return tx.commit();
        }
        if let Some(old) = server_copy(&tx, id)?.filter(|old| !old.same(copy)) {
            queue_removal(&tx, upload.account_id, &old)?;
        }
        if let (Some(mailbox), Some(uid)) = (&copy.mailbox, copy.uid) {
            // A stale row holding this location would break the unique index.
            tx.execute(
                "UPDATE messages SET mailbox = NULL, uid = NULL
                 WHERE account_id = ?1 AND mailbox = ?2 AND uid = ?3 AND id != ?4",
                params![upload.account_id, mailbox, uid, id],
            )?;
        }
        tx.execute(
            "UPDATE messages SET remote_id = ?2, mailbox = ?3, uid = ?4, message_id = ?5,
                remote_place = 'folder', attachments = ?6
             WHERE id = ?1",
            params![
                id,
                copy.remote_id,
                copy.mailbox,
                copy.uid,
                copy.message_id,
                json(attachments)
            ],
        )?;
        tx.execute(
            "INSERT INTO folder_members (folder_id, message_id, uid)
             SELECT id, ?2, ?3 FROM folders
             WHERE account_id = ?1 AND role = 'drafts' AND (?4 IS NULL OR remote_id = ?4)
             ORDER BY position LIMIT 1
             ON CONFLICT (folder_id, message_id) DO UPDATE SET uid = excluded.uid",
            params![upload.account_id, id, copy.uid, copy.mailbox],
        )?;
        tx.execute(
            "DELETE FROM draft_uploads WHERE message_id = ?1 AND saved_at = ?2",
            params![id, upload.saved_at],
        )?;
        tx.commit()
    }

    /// Gives up on uploading a draft (the server refuses it): it stays here.
    pub fn drop_draft_upload(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM draft_uploads WHERE message_id = ?1", [id])?;
        Ok(())
    }

    /// Server copies of the account's drafts waiting to be deleted.
    pub fn draft_removals(&self, account_id: i64) -> Result<Vec<DraftRemoval>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, mailbox, uid, remote_id, message_id FROM draft_removals
             WHERE account_id = ?1 ORDER BY id",
        )?;
        stmt.query_map([account_id], |r| {
            Ok(DraftRemoval {
                id: r.get(0)?,
                copy: DraftCopy {
                    mailbox: r.get(1)?,
                    uid: r.get(2)?,
                    remote_id: r.get(3)?,
                    message_id: r.get(4)?,
                },
            })
        })?
        .collect()
    }

    /// The server copy is gone (or won't go): forget it.
    pub fn draft_removed(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM draft_removals WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Whether a sync round has drafts to upload or delete.
    pub fn has_draft_work(&self, account_id: i64, settled_before: i64) -> Result<bool> {
        self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM draft_removals WHERE account_id = ?1)
                 OR EXISTS (SELECT 1 FROM draft_uploads u JOIN messages m ON m.id = u.message_id
                            WHERE m.account_id = ?1 AND u.saved_at <= ?2
                              AND u.message_id NOT IN
                                  (SELECT draft_message FROM outbox WHERE draft_message IS NOT NULL))",
            params![account_id, settled_before],
            |r| r.get(0),
        )
    }

    /// The Message-ID a stored message replies to.
    pub fn in_reply_to(&self, id: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT in_reply_to FROM messages WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }

    /// The newest stored message of the account with this Message-ID.
    pub fn find_message_id(&self, account_id: i64, message_id: &str) -> Result<Option<i64>> {
        self.conn
            .query_row(
                "SELECT id FROM messages WHERE account_id = ?1 AND message_id = ?2
                 ORDER BY date DESC LIMIT 1",
                params![account_id, message_id],
                |r| r.get(0),
            )
            .optional()
    }
}

fn is_draft_in(conn: &Connection, id: i64) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM draft_uploads WHERE message_id = ?1)
             OR EXISTS (SELECT 1 FROM folder_members fm JOIN folders f ON f.id = fm.folder_id
                        WHERE fm.message_id = ?1 AND f.role = 'drafts')",
        [id],
        |r| r.get(0),
    )
}

/// Where the server keeps a draft, if it's there: by its Graph id, or in
/// the Drafts folder by UID (else Message-ID); a draft found elsewhere first
/// (Gmail's All Mail) is still deleted from Drafts.
fn server_copy(conn: &Connection, id: i64) -> Result<Option<DraftCopy>> {
    type Row = (
        Option<String>,
        Option<String>,
        Option<u32>,
        Option<String>,
        Option<String>,
        Option<u32>,
    );
    let row: Option<Row> = conn
        .query_row(
            "SELECT m.remote_id, m.mailbox, m.uid, m.message_id, f.remote_id, fm.uid
             FROM messages m
             LEFT JOIN folder_members fm ON fm.message_id = m.id AND fm.folder_id IN
                 (SELECT id FROM folders WHERE account_id = m.account_id AND role = 'drafts')
             LEFT JOIN folders f ON f.id = fm.folder_id
             WHERE m.id = ?1 LIMIT 1",
            [id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((remote_id, mailbox, uid, message_id, drafts, drafts_uid)) = row else {
        return Ok(None);
    };
    let (mailbox, uid) = match drafts {
        Some(drafts) => {
            let uid = drafts_uid.or(uid.filter(|_| mailbox.as_deref() == Some(drafts.as_str())));
            (Some(drafts), uid)
        }
        None => (mailbox, uid),
    };
    let copy = DraftCopy {
        mailbox,
        uid,
        remote_id,
        message_id,
    };
    Ok(copy.exists().then_some(copy))
}

fn queue_removal(conn: &Connection, account_id: i64, copy: &DraftCopy) -> Result<()> {
    if !copy.exists() {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO draft_removals (account_id, mailbox, uid, remote_id, message_id)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            account_id,
            copy.mailbox,
            copy.uid,
            copy.remote_id,
            copy.message_id
        ],
    )?;
    Ok(())
}

/// Deletes a draft's row (its queued save and files go with it) after
/// queuing its server copy for deleting.
fn discard_in(conn: &Connection, id: i64) -> Result<()> {
    let account_id: Option<i64> = conn
        .query_row("SELECT account_id FROM messages WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .optional()?;
    let Some(account_id) = account_id else {
        return Ok(());
    };
    if let Some(copy) = server_copy(conn, id)? {
        queue_removal(conn, account_id, &copy)?;
    }
    conn.execute("DELETE FROM messages WHERE id = ?1", [id])?;
    Ok(())
}

#[cfg(test)]
mod tests;
