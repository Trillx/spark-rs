//! Conversations: which stored messages the thread view shows together.
//!
//! A message joins the conversation of every stored message of its account
//! it shares a key with: a Message-ID it has or replies to (its own,
//! In-Reply-To and References), or the server's conversation id (Microsoft
//! Graph). Message-ID keys only match between messages whose subjects agree
//! once "Re:"/"Fwd:" prefixes and list tags are gone, as in Gmail, so a reused
//! or mistaken References header doesn't merge unrelated mail. When a message
//! links two conversations (a reply that arrived before what it answers),
//! they merge. A conversation's id is the row id of the message it started
//! with.

use std::collections::BTreeSet;

use rusqlite::{Connection, params};

use crate::Result;

/// Version 6: each message's conversation, and the keys that link them.
pub(crate) const SCHEMA_CONVERSATIONS: &str = "
ALTER TABLE messages ADD COLUMN conversation INTEGER;
CREATE INDEX messages_conversation ON messages(account_id, conversation);

CREATE TABLE message_keys (
    message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    account_id INTEGER NOT NULL,
    key TEXT NOT NULL,
    PRIMARY KEY (message_id, key)
);
CREATE INDEX message_keys_key ON message_keys(account_id, key);
";

/// The headers that place a message in a conversation, as stored.
pub(crate) struct Threading<'a> {
    pub message_id: Option<&'a str>,
    pub in_reply_to: Option<&'a str>,
    pub references: Option<&'a str>,
    /// The server's conversation id.
    pub thread_id: Option<&'a str>,
    pub subject: &'a str,
}

/// Puts stored message `id` in its conversation, merging the ones it links.
pub(crate) fn link(
    conn: &Connection,
    id: i64,
    account_id: i64,
    threading: &Threading,
) -> Result<()> {
    let mut found = BTreeSet::<i64>::new();
    {
        let mut find = conn.prepare_cached(
            "SELECT m.conversation FROM message_keys k JOIN messages m ON m.id = k.message_id
             WHERE k.account_id = ?1 AND k.key = ?2 AND k.message_id != ?3
               AND m.conversation IS NOT NULL",
        )?;
        let mut insert = conn.prepare_cached(
            "INSERT OR IGNORE INTO message_keys (message_id, account_id, key) VALUES (?1, ?2, ?3)",
        )?;
        for key in keys(threading) {
            for conversation in find.query_map(params![account_id, key, id], |r| r.get(0))? {
                found.insert(conversation?);
            }
            insert.execute(params![id, account_id, key])?;
        }
    }
    let target = found.first().map_or(id, |&first| first.min(id));
    conn.execute(
        "UPDATE messages SET conversation = ?1 WHERE id = ?2",
        params![target, id],
    )?;
    for merged in found.into_iter().filter(|&c| c != target) {
        conn.execute(
            "UPDATE messages SET conversation = ?1 WHERE account_id = ?2 AND conversation = ?3",
            params![target, account_id, merged],
        )?;
    }
    Ok(())
}

/// Places mail stored before conversations existed, oldest first.
pub(crate) fn backfill(conn: &Connection) -> Result<()> {
    type Row = (
        i64,
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
    );
    let rows = conn
        .prepare(
            "SELECT id, account_id, message_id, in_reply_to, refs, thread_id, subject
             FROM messages ORDER BY id",
        )?
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })?
        .collect::<Result<Vec<Row>>>()?;
    for (id, account_id, message_id, in_reply_to, refs, thread_id, subject) in &rows {
        let threading = Threading {
            message_id: message_id.as_deref(),
            in_reply_to: in_reply_to.as_deref(),
            references: refs.as_deref(),
            thread_id: thread_id.as_deref(),
            subject,
        };
        link(conn, *id, *account_id, &threading)?;
    }
    Ok(())
}

/// `<message-id>topic` for each Message-ID, `#id` for the server's
/// conversation id; the brackets keep the two kinds apart.
fn keys(t: &Threading) -> Vec<String> {
    let topic = topic(t.subject);
    let ids = t
        .message_id
        .into_iter()
        .chain(t.in_reply_to)
        .chain(t.references.into_iter().flat_map(str::split_whitespace))
        .map(|id| id.trim().trim_matches(['<', '>']))
        .filter(|id| !id.is_empty());
    let mut keys: Vec<String> = ids.map(|id| format!("<{id}>{topic}")).collect();
    keys.extend(
        t.thread_id
            .filter(|id| !id.is_empty())
            .map(|id| format!("#{id}")),
    );
    keys.sort();
    keys.dedup();
    keys
}

/// Reply and forward prefixes in the languages mail clients write them in.
const PREFIXES: [&str; 14] = [
    "re", "fw", "fwd", "aw", "wg", "sv", "vs", "antw", "tr", "rv", "ref", "odp", "enc", "res",
];

/// What a subject is about: without "Re:", "Fwd:", "AW:", "RE[2]:" and
/// "[list]" tags in front, in lowercase with single spaces.
pub(crate) fn topic(subject: &str) -> String {
    let mut rest = subject.trim();
    while let Some(after) = strip_tag(rest).or_else(|| strip_prefix(rest)) {
        rest = after.trim_start();
    }
    rest.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// "[list] rest" → "rest".
fn strip_tag(s: &str) -> Option<&str> {
    let inner = s.strip_prefix('[')?;
    let close = inner.find(']')?;
    Some(&inner[close + 1..])
}

/// "Re: rest", "RE[2]: rest", "Fwd : rest" → "rest".
fn strip_prefix(s: &str) -> Option<&str> {
    let end = s
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(s.len());
    let (word, after) = s.split_at(end);
    if !PREFIXES.iter().any(|p| word.eq_ignore_ascii_case(p)) {
        return None;
    }
    let mut after = after.trim_start();
    if let Some(count) = after.strip_prefix('[').or_else(|| after.strip_prefix('(')) {
        let close = count.find([']', ')'])?;
        if !count[..close].chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        after = count[close + 1..].trim_start();
    }
    after.strip_prefix(':').or_else(|| after.strip_prefix('：'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topics_drop_reply_prefixes_and_list_tags() {
        for subject in [
            "Q4 roadmap",
            "Re: Q4 roadmap",
            "RE: RE: q4  roadmap",
            "Fwd: Re: Q4 roadmap",
            "AW: WG: Q4 roadmap",
            "Re[2]: Q4 roadmap",
            "[team] Re: Q4 roadmap",
            "Re : Q4 roadmap ",
        ] {
            assert_eq!(topic(subject), "q4 roadmap", "{subject}");
        }
        // Only whole words before a colon are prefixes.
        assert_eq!(topic("Regarding: the plan"), "regarding: the plan");
        assert_eq!(topic("Re-org notes"), "re-org notes");
        assert_eq!(topic(""), "");
    }

    #[test]
    fn keys_pair_message_ids_with_the_topic() {
        let keys = keys(&Threading {
            message_id: Some("2@x.com"),
            in_reply_to: Some("<1@x.com>"),
            references: Some("0@x.com 1@x.com"),
            thread_id: Some("AAQk"),
            subject: "Re: Plan",
        });
        assert_eq!(
            keys,
            ["#AAQk", "<0@x.com>plan", "<1@x.com>plan", "<2@x.com>plan"]
        );
    }
}
