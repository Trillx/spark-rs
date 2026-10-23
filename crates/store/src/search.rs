//! Full-text search over stored mail: an SQLite FTS5 index of each message's
//! sender, recipients, subject and text, which triggers keep in step with the
//! messages table. Words match the start of words in the mail, ignoring case
//! and accents, so "invo" finds "Invoice" and "cafe" finds "Café".

use crate::{MESSAGE_COLUMNS, Message, Result, Store, message_row};

/// Version 9: the search index, filled from the mail already stored.
pub(crate) const SCHEMA_SEARCH: &str = "
CREATE VIRTUAL TABLE messages_fts USING fts5(
    from_name, from_address, to_list, cc_list, subject, text_body,
    content='messages', content_rowid='id',
    tokenize='unicode61 remove_diacritics 2', prefix='2 3'
);

CREATE TRIGGER messages_fts_insert AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts (rowid, from_name, from_address, to_list, cc_list, subject, text_body)
    VALUES (new.id, new.from_name, new.from_address, new.to_list, new.cc_list, new.subject,
            new.text_body);
END;

CREATE TRIGGER messages_fts_delete AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts (messages_fts, rowid, from_name, from_address, to_list, cc_list,
                              subject, text_body)
    VALUES ('delete', old.id, old.from_name, old.from_address, old.to_list, old.cc_list,
            old.subject, old.text_body);
END;

CREATE TRIGGER messages_fts_update
AFTER UPDATE OF from_name, from_address, to_list, cc_list, subject, text_body ON messages BEGIN
    INSERT INTO messages_fts (messages_fts, rowid, from_name, from_address, to_list, cc_list,
                              subject, text_body)
    VALUES ('delete', old.id, old.from_name, old.from_address, old.to_list, old.cc_list,
            old.subject, old.text_body);
    INSERT INTO messages_fts (rowid, from_name, from_address, to_list, cc_list, subject, text_body)
    VALUES (new.id, new.from_name, new.from_address, new.to_list, new.cc_list, new.subject,
            new.text_body);
END;

INSERT INTO messages_fts (messages_fts) VALUES ('rebuild');
";

impl Store {
    /// Messages whose sender, recipients, subject or text has every word
    /// of `text`, newest first, without bodies. A word matches the start of
    /// a word, and "quoted words" must appear together. Text with no letters
    /// or digits finds nothing. Like `messages`, it skips drafts waiting in
    /// the outbox.
    pub fn search(&self, text: &str) -> Result<Vec<Message>> {
        let Some(query) = fts_query(text) else {
            return Ok(Vec::new());
        };
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages
             WHERE id IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?1)
               AND id NOT IN (SELECT draft_message FROM outbox WHERE draft_message IS NOT NULL)
             ORDER BY date DESC"
        ))?;
        stmt.query_map([query], message_row)?.collect()
    }
}

/// The FTS5 query for what the user typed: each word as a prefix, each
/// quoted part as a phrase, all of them required. Every term is an FTS5
/// string, so operators and punctuation in the text are taken literally.
/// An unclosed quote runs to the end.
pub(crate) fn fts_query(text: &str) -> Option<String> {
    let mut terms = Vec::new();
    for (i, part) in text.split('"').enumerate() {
        if i % 2 == 1 {
            terms.extend(phrase(part));
        } else {
            terms.extend(part.split_whitespace().filter_map(phrase).map(|t| t + "*"));
        }
    }
    (!terms.is_empty()).then(|| terms.join(" "))
}

/// `text` (which has no double quotes) as an FTS5 string, unless it has
/// nothing the index keeps.
fn phrase(text: &str) -> Option<String> {
    text.chars()
        .any(char::is_alphanumeric)
        .then(|| format!("\"{}\"", text.trim()))
}
