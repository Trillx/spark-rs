//! Full-text search over stored mail: an SQLite FTS5 index of each message's
//! sender, recipients, subject and text, which triggers keep in step with the
//! messages table. Words match the start of words in the mail, ignoring case
//! and accents, so "invo" finds "Invoice" and "cafe" finds "Café". `Query` is
//! the search syntax around it, which the mail crate also puts to servers.

use crate::{MESSAGE_COLUMNS, Message, Result, Store, message_row};

/// A search as the search field and the agents' search tool take it: free
/// text plus `from:`, `has:attachment` and `is:unread`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    /// What's left for `Store::search`.
    pub text: String,
    /// Part of the sender's name or address, lowercase.
    pub from: Option<String>,
    pub has_attachment: bool,
    pub unread_only: bool,
}

/// A word of a search's free text, or a quoted part of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Term {
    Word(String),
    Phrase(String),
}

impl Query {
    pub fn parse(query: &str) -> Self {
        let mut parsed = Query::default();
        let mut words = Vec::new();
        for word in query.split_whitespace() {
            let lower = word.to_lowercase();
            if let Some(from) = lower.strip_prefix("from:").filter(|f| !f.is_empty()) {
                parsed.from = Some(from.to_owned());
            } else if lower == "has:attachment" || lower == "has:attachments" {
                parsed.has_attachment = true;
            } else if lower == "is:unread" {
                parsed.unread_only = true;
            } else {
                words.push(word);
            }
        }
        parsed.text = words.join(" ");
        parsed
    }

    /// The free text's words, and its quoted parts as phrases.
    pub fn terms(&self) -> Vec<Term> {
        terms(&self.text)
    }

    /// Whether a message the free text found passes the filters.
    pub fn matches(&self, m: &Message) -> bool {
        let from_ok = self.from.as_ref().is_none_or(|from| {
            format!("{} {}", m.from_name, m.from_address)
                .to_lowercase()
                .contains(from)
        });
        from_ok
            && (!self.has_attachment || m.attachment.is_some())
            && (!self.unread_only || m.unread)
    }
}

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

    /// The ids of the messages `search` finds, all of them, in no
    /// particular order: for the UI, which filters the messages it has,
    /// it's the index alone, without reading or sorting the messages.
    pub fn search_ids(&self, text: &str) -> Result<Vec<i64>> {
        let Some(query) = fts_query(text) else {
            return Ok(Vec::new());
        };
        let mut stmt = self.conn.prepare_cached(SEARCH_IDS)?;
        stmt.query_map([query], |r| r.get(0))?.collect()
    }
}

impl Store {
    /// The ids of the messages `search` finds, best match first by FTS5's
    /// bm25, where words in the subject and the sender count most.
    pub fn search_ranked(&self, text: &str) -> Result<Vec<i64>> {
        let Some(query) = fts_query(text) else {
            return Ok(Vec::new());
        };
        let mut stmt = self.conn.prepare_cached(
            "SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?1
               AND rowid NOT IN (SELECT draft_message FROM outbox WHERE draft_message IS NOT NULL)
             ORDER BY bm25(messages_fts, 4.0, 4.0, 2.0, 1.0, 6.0, 1.0)",
        )?;
        stmt.query_map([query], |r| r.get(0))?.collect()
    }
}

/// `Store::search_ids` for the FTS query `?1`: what `search` asks, but only
/// the ids, unsorted.
pub(crate) const SEARCH_IDS: &str = "SELECT id FROM messages
     WHERE id IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?1)
       AND id NOT IN (SELECT draft_message FROM outbox WHERE draft_message IS NOT NULL)";

/// The FTS5 query for what the user typed: each word as a prefix, each
/// quoted part as a phrase, all of them required. Every term is an FTS5
/// string, so operators and punctuation in the text are taken literally.
pub(crate) fn fts_query(text: &str) -> Option<String> {
    let terms: Vec<String> = terms(text)
        .into_iter()
        .map(|t| match t {
            Term::Word(word) => format!("\"{word}\"*"),
            Term::Phrase(phrase) => format!("\"{phrase}\""),
        })
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

/// The words of `text`, and its quoted parts as phrases, leaving out those
/// without letters or digits (the index keeps none). An unclosed quote runs
/// to the end.
fn terms(text: &str) -> Vec<Term> {
    let mut terms = Vec::new();
    for (i, part) in text.split('"').enumerate() {
        if i % 2 == 1 {
            terms.extend(indexed(part).map(|p| Term::Phrase(p.to_owned())));
        } else {
            terms.extend(
                part.split_whitespace()
                    .filter_map(indexed)
                    .map(|w| Term::Word(w.to_owned())),
            );
        }
    }
    terms
}

/// `text` trimmed, unless it has nothing the index keeps.
fn indexed(text: &str) -> Option<&str> {
    text.chars().any(char::is_alphanumeric).then(|| text.trim())
}
