//! Email signatures, and which one each sending address uses. Defaults are
//! kept per address rather than per account, so an alias can have its own.
//! An address without one uses its most recent signature: the one it last
//! sent, else the newest.

use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::{Result, Store};

/// Version 10: signatures, each address's default, and the one it last sent.
pub(crate) const SCHEMA_SIGNATURES: &str = "
CREATE TABLE signatures (
    id INTEGER PRIMARY KEY,
    html TEXT NOT NULL,
    text TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

-- An address's default when it isn't Most Recent: a signature, or NULL for none.
CREATE TABLE signature_defaults (
    mailbox TEXT PRIMARY KEY COLLATE NOCASE,
    signature_id INTEGER REFERENCES signatures(id) ON DELETE CASCADE
);

CREATE TABLE signature_usage (
    mailbox TEXT PRIMARY KEY COLLATE NOCASE,
    signature_id INTEGER NOT NULL REFERENCES signatures(id) ON DELETE CASCADE,
    used_at INTEGER NOT NULL
);
";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Signature {
    pub id: i64,
    /// Sanitized by the mail crate before it's stored.
    pub html: String,
    /// The HTML as plain text: the text/plain version, and its name in lists.
    pub text: String,
}

/// Which signature an address starts new mail with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "id", rename_all = "camelCase")]
pub enum DefaultSignature {
    /// The one it last sent, else the newest.
    #[default]
    MostRecent,
    None,
    Custom(i64),
}

impl Store {
    /// Oldest first.
    pub fn signatures(&self) -> Result<Vec<Signature>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, html, text FROM signatures ORDER BY id")?;
        stmt.query_map([], signature_row)?.collect()
    }

    pub fn signature(&self, id: i64) -> Result<Option<Signature>> {
        self.conn
            .query_row(
                "SELECT id, html, text FROM signatures WHERE id = ?1",
                [id],
                signature_row,
            )
            .optional()
    }

    pub fn add_signature(&self, html: &str, text: &str, now: i64) -> Result<Signature> {
        self.conn.execute(
            "INSERT INTO signatures (html, text, created_at) VALUES (?1, ?2, ?3)",
            params![html, text, now],
        )?;
        Ok(Signature {
            id: self.conn.last_insert_rowid(),
            html: html.to_owned(),
            text: text.to_owned(),
        })
    }

    /// False if it's gone.
    pub fn update_signature(&self, id: i64, html: &str, text: &str) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE signatures SET html = ?2, text = ?3 WHERE id = ?1",
            params![id, html, text],
        )?;
        Ok(changed > 0)
    }

    /// Addresses that defaulted to it go back to Most Recent.
    pub fn remove_signature(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM signatures WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn default_signature(&self, mailbox: &str) -> Result<DefaultSignature> {
        let stored: Option<Option<i64>> = self
            .conn
            .query_row(
                "SELECT signature_id FROM signature_defaults WHERE mailbox = ?1",
                [mailbox],
                |r| r.get(0),
            )
            .optional()?;
        Ok(match stored {
            None => DefaultSignature::MostRecent,
            Some(None) => DefaultSignature::None,
            Some(Some(id)) => DefaultSignature::Custom(id),
        })
    }

    /// A signature that's gone leaves the address on Most Recent.
    pub fn set_default_signature(&self, mailbox: &str, default: DefaultSignature) -> Result<()> {
        match default {
            DefaultSignature::MostRecent => {
                self.conn.execute(
                    "DELETE FROM signature_defaults WHERE mailbox = ?1",
                    [mailbox],
                )?;
            }
            DefaultSignature::None => {
                self.conn.execute(
                    "INSERT OR REPLACE INTO signature_defaults (mailbox, signature_id) VALUES (?1, NULL)",
                    [mailbox],
                )?;
            }
            DefaultSignature::Custom(id) => {
                self.conn.execute(
                    "DELETE FROM signature_defaults WHERE mailbox = ?1",
                    [mailbox],
                )?;
                self.conn.execute(
                    "INSERT INTO signature_defaults (mailbox, signature_id)
                     SELECT ?1, id FROM signatures WHERE id = ?2",
                    params![mailbox, id],
                )?;
            }
        }
        Ok(())
    }

    /// `mailbox` sent signature `id`, which Most Recent now means. A
    /// signature deleted since is ignored.
    pub fn use_signature(&self, mailbox: &str, id: i64, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO signature_usage (mailbox, signature_id, used_at)
             SELECT ?1, id, ?3 FROM signatures WHERE id = ?2",
            params![mailbox, id, now],
        )?;
        Ok(())
    }

    /// The signature new mail from `mailbox` starts with, if any.
    pub fn signature_for(&self, mailbox: &str) -> Result<Option<Signature>> {
        match self.default_signature(mailbox)? {
            DefaultSignature::None => Ok(None),
            DefaultSignature::Custom(id) => self.signature(id),
            DefaultSignature::MostRecent => self
                .conn
                .query_row(
                    "SELECT id, html, text FROM signatures
                     WHERE id = COALESCE(
                         (SELECT signature_id FROM signature_usage WHERE mailbox = ?1),
                         (SELECT MAX(id) FROM signatures))",
                    [mailbox],
                    signature_row,
                )
                .optional(),
        }
    }

    /// What `mailbox` sent in the conversation of message `id`, newest
    /// first: where a reply looks for the signature used before.
    pub fn sent_in_conversation(&self, id: i64, mailbox: &str, limit: usize) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT m.id FROM messages m
             WHERE COALESCE(m.conversation, m.id) =
                   (SELECT COALESCE(conversation, id) FROM messages WHERE id = ?1)
               AND m.from_address = ?2 COLLATE NOCASE
               AND NOT EXISTS (SELECT 1 FROM folder_members fm JOIN folders f ON f.id = fm.folder_id
                               WHERE fm.message_id = m.id AND f.role = 'drafts')
             ORDER BY m.date DESC LIMIT ?3",
        )?;
        stmt.query_map(params![id, mailbox, limit as i64], |r| r.get(0))?
            .collect()
    }
}

fn signature_row(r: &Row) -> Result<Signature> {
    Ok(Signature {
        id: r.get(0)?,
        html: r.get(1)?,
        text: r.get(2)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    #[test]
    fn most_recent_is_the_last_one_sent_else_the_newest() {
        let store = store();
        assert_eq!(store.signature_for("me@x.com").unwrap(), None);
        let work = store.add_signature("<b>Work</b>", "Work", 1).unwrap();
        let home = store.add_signature("Home", "Home", 2).unwrap();
        assert_eq!(
            store.default_signature("me@x.com").unwrap(),
            DefaultSignature::MostRecent
        );
        assert_eq!(store.signature_for("me@x.com").unwrap(), Some(home.clone()));
        store.use_signature("Me@X.com", work.id, 3).unwrap();
        assert_eq!(
            store.signature_for("me@x.com").unwrap(),
            Some(work.clone()),
            "addresses ignore case"
        );
        assert_eq!(
            store.signature_for("other@x.com").unwrap(),
            Some(home.clone())
        );
        // Deleting the one last sent falls back to the newest.
        store.remove_signature(work.id).unwrap();
        assert_eq!(store.signature_for("me@x.com").unwrap(), Some(home));
        store.use_signature("me@x.com", work.id, 4).unwrap();
        assert_eq!(
            store.signatures().unwrap().len(),
            1,
            "a deleted one isn't brought back"
        );
    }

    #[test]
    fn an_address_can_default_to_none_or_one_signature() {
        let store = store();
        let work = store.add_signature("Work", "Work", 1).unwrap();
        let home = store.add_signature("Home", "Home", 2).unwrap();
        store
            .set_default_signature("me@x.com", DefaultSignature::Custom(work.id))
            .unwrap();
        store.use_signature("me@x.com", home.id, 3).unwrap();
        assert_eq!(
            store.signature_for("me@x.com").unwrap(),
            Some(work.clone()),
            "the default wins over the last sent"
        );
        store
            .set_default_signature("ME@x.com", DefaultSignature::None)
            .unwrap();
        assert_eq!(
            store.default_signature("me@x.com").unwrap(),
            DefaultSignature::None
        );
        assert_eq!(store.signature_for("me@x.com").unwrap(), None);
        store
            .set_default_signature("me@x.com", DefaultSignature::Custom(work.id))
            .unwrap();
        store.remove_signature(work.id).unwrap();
        assert_eq!(
            store.default_signature("me@x.com").unwrap(),
            DefaultSignature::MostRecent
        );
        store
            .set_default_signature("me@x.com", DefaultSignature::Custom(99))
            .unwrap();
        assert_eq!(
            store.default_signature("me@x.com").unwrap(),
            DefaultSignature::MostRecent,
            "a missing one is ignored"
        );
        store
            .set_default_signature("me@x.com", DefaultSignature::MostRecent)
            .unwrap();
        assert_eq!(store.signature_for("me@x.com").unwrap(), Some(home));
    }

    #[test]
    fn signatures_are_edited_in_place() {
        let store = store();
        let s = store.add_signature("A", "A", 1).unwrap();
        assert!(store.update_signature(s.id, "<i>B</i>", "B").unwrap());
        assert_eq!(store.signature(s.id).unwrap().unwrap().html, "<i>B</i>");
        assert!(!store.update_signature(99, "C", "C").unwrap());
    }

    #[test]
    fn defaults_serialize_for_the_ui() {
        let json = |d| serde_json::to_string(&d).unwrap();
        assert_eq!(
            json(DefaultSignature::MostRecent),
            r#"{"type":"mostRecent"}"#
        );
        assert_eq!(json(DefaultSignature::None), r#"{"type":"none"}"#);
        assert_eq!(
            json(DefaultSignature::Custom(3)),
            r#"{"type":"custom","id":3}"#
        );
        let custom: DefaultSignature = serde_json::from_str(r#"{"type":"custom","id":3}"#).unwrap();
        assert_eq!(custom, DefaultSignature::Custom(3));
    }
}
