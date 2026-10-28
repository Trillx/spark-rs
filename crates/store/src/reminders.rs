//! Spark's "Remind me": an email sent with a reminder comes back to the
//! Inbox at the chosen time, unless (with "if no reply") someone answered
//! it first, as far as the last sync knows. It comes back here only: the email isn't moved on the server,
//! where it stays in Sent. Until Done, the stored email it's about (the
//! sent one, else the one it replied to) shows in the Inbox.

use std::collections::HashMap;

use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::{Result, Store};

/// Version 16: reminders on sent mail.
pub(crate) const SCHEMA_REMINDERS: &str = "
CREATE TABLE reminders (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    -- The sent email's Message-ID, when it's known (not for Graph's sends).
    message_id TEXT,
    -- The stored email it replied to, if any.
    reply_to INTEGER REFERENCES messages(id) ON DELETE SET NULL,
    subject TEXT NOT NULL,
    to_list TEXT NOT NULL,
    sent_at INTEGER NOT NULL,
    remind_at INTEGER NOT NULL,
    if_no_reply INTEGER NOT NULL,
    -- 'pending' until remind_at; then 'due' (in the Inbox) until Done ('done').
    state TEXT NOT NULL DEFAULT 'pending',
    -- The stored email it came back as, once due.
    message_row INTEGER REFERENCES messages(id) ON DELETE SET NULL
);
CREATE INDEX reminders_due ON reminders(state, remind_at);
-- The Inbox looks a due reminder up by its email, for every listed message.
CREATE INDEX reminders_message ON reminders(message_row, remind_at) WHERE state = 'due';
";

/// How long a reminder waits past its time for its account to sync, so a
/// reply already on the server is seen first; after that it comes due anyway.
const SYNC_GRACE_MS: i64 = 10 * 60_000;

/// A reminder to set on an email that was just sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewReminder<'a> {
    pub account_id: i64,
    pub message_id: Option<&'a str>,
    pub reply_to: Option<i64>,
    pub subject: &'a str,
    /// Who it went to, for the Reminders list.
    pub to: &'a str,
    pub sent_at: i64,
    pub remind_at: i64,
    /// Only if nobody else wrote in the conversation since it was sent.
    pub if_no_reply: bool,
}

/// A reminder as it was stored, all of it (`Store::remove_reminder`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemovedReminder {
    pub id: i64,
    pub account_id: i64,
    message_id: Option<String>,
    reply_to: Option<i64>,
    subject: String,
    to_list: String,
    sent_at: i64,
    pub remind_at: i64,
    if_no_reply: bool,
    state: String,
    message_row: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reminder {
    pub id: i64,
    pub account_id: i64,
    pub subject: String,
    pub to: String,
    pub remind_at: i64,
    pub if_no_reply: bool,
    /// It came due and waits in the Inbox.
    pub due: bool,
    /// The stored email it came back as, once due (and found).
    pub message: Option<i64>,
}

const COLUMNS: &str =
    "id, account_id, subject, to_list, remind_at, if_no_reply, state = 'due', message_row";

fn reminder_row(r: &Row) -> Result<Reminder> {
    Ok(Reminder {
        id: r.get(0)?,
        account_id: r.get(1)?,
        subject: r.get(2)?,
        to: r.get(3)?,
        remind_at: r.get(4)?,
        if_no_reply: r.get(5)?,
        due: r.get(6)?,
        message: r.get(7)?,
    })
}

impl Store {
    pub fn add_reminder(&self, new: &NewReminder) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO reminders (account_id, message_id, reply_to, subject, to_list, sent_at,
                 remind_at, if_no_reply)
             VALUES (?1, ?2, (SELECT id FROM messages WHERE id = ?3), ?4, ?5, ?6, ?7, ?8)",
            params![
                new.account_id,
                new.message_id.map(|id| id.trim_matches(['<', '>'])),
                new.reply_to,
                new.subject,
                new.to,
                new.sent_at,
                new.remind_at,
                new.if_no_reply
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Reminders still to come and those waiting in the Inbox, soonest first.
    pub fn reminders(&self) -> Result<Vec<Reminder>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM reminders WHERE state != 'done' ORDER BY remind_at"
        ))?;
        stmt.query_map([], reminder_row)?.collect()
    }

    pub fn reminder(&self, id: i64) -> Result<Option<Reminder>> {
        self.conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM reminders WHERE id = ?1"),
                [id],
                reminder_row,
            )
            .optional()
    }

    /// Moves a reminder to `at` (it's to come again), or with `None`
    /// removes it.
    pub fn set_reminder(&self, id: i64, at: Option<i64>) -> Result<()> {
        match at {
            Some(at) => self.conn.execute(
                "UPDATE reminders SET remind_at = ?2, state = 'pending', message_row = NULL
                 WHERE id = ?1",
                params![id, at],
            )?,
            None => self
                .conn
                .execute("DELETE FROM reminders WHERE id = ?1", [id])?,
        };
        Ok(())
    }

    /// Removes a reminder, returning all of it so `restore_reminder` can
    /// put it back.
    pub fn remove_reminder(&mut self, id: i64) -> Result<Option<RemovedReminder>> {
        let tx = self.conn.transaction()?;
        let removed = tx
            .query_row(
                "SELECT id, account_id, message_id, reply_to, subject, to_list, sent_at,
                        remind_at, if_no_reply, state, message_row
                 FROM reminders WHERE id = ?1",
                [id],
                |r| {
                    Ok(RemovedReminder {
                        id: r.get(0)?,
                        account_id: r.get(1)?,
                        message_id: r.get(2)?,
                        reply_to: r.get(3)?,
                        subject: r.get(4)?,
                        to_list: r.get(5)?,
                        sent_at: r.get(6)?,
                        remind_at: r.get(7)?,
                        if_no_reply: r.get(8)?,
                        state: r.get(9)?,
                        message_row: r.get(10)?,
                    })
                },
            )
            .optional()?;
        tx.execute("DELETE FROM reminders WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(removed)
    }

    /// Puts back a reminder `remove_reminder` took out, under its own id;
    /// false if that id is taken again or its account is gone. Emails it
    /// pointed to that are gone since are left out.
    pub fn restore_reminder(&self, r: &RemovedReminder) -> Result<bool> {
        let added = self.conn.execute(
            "INSERT OR IGNORE INTO reminders (id, account_id, message_id, reply_to, subject,
                 to_list, sent_at, remind_at, if_no_reply, state, message_row)
             SELECT ?1, ?2, ?3, (SELECT id FROM messages WHERE id = ?4), ?5, ?6, ?7, ?8, ?9,
                    ?10, (SELECT id FROM messages WHERE id = ?11)
             WHERE EXISTS (SELECT 1 FROM accounts WHERE id = ?2)",
            params![
                r.id,
                r.account_id,
                r.message_id,
                r.reply_to,
                r.subject,
                r.to_list,
                r.sent_at,
                r.remind_at,
                r.if_no_reply,
                r.state,
                r.message_row
            ],
        )?;
        Ok(added == 1)
    }

    /// The reminders to come or waiting on stored email `message`: one set
    /// on it when it was sent, or the one it came back as.
    pub fn reminders_on(&self, message: i64) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT r.id FROM reminders r JOIN messages m ON m.id = ?1
             WHERE r.state IN ('pending', 'due')
               AND (r.message_row = m.id
                    OR (r.account_id = m.account_id AND m.message_id IS NOT NULL
                        AND trim(r.message_id, '<>') = trim(m.message_id, '<>')))",
        )?;
        stmt.query_map([message], |r| r.get(0))?.collect()
    }

    /// Done on a reminder in the Inbox, or (`false`) its undo.
    pub fn finish_reminder(&self, id: i64, done: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE reminders SET state = CASE WHEN ?2 THEN 'done' ELSE 'due' END
             WHERE id = ?1 AND state IN ('due', 'done')",
            params![id, done],
        )?;
        Ok(())
    }

    /// Reminders whose time has come, once the account's mail is known to
    /// be current: synced since (`synced`, by account, Unix ms), or
    /// `SYNC_GRACE_MS` later regardless. Each comes due, in the Inbox as the
    /// stored email it's about (the sent one, by its Message-ID, else the
    /// one it replied to); or with "if no reply" and an answer, goes. Due
    /// ones keep looking for their email if it wasn't stored yet, and "if
    /// no reply" ones go when an answer turns up later. Returns the ones
    /// that just came due.
    pub fn wake_reminders(
        &mut self,
        now: i64,
        synced: &HashMap<i64, i64>,
    ) -> Result<Vec<Reminder>> {
        let tx = self.conn.transaction()?;
        type Live = (
            i64,
            i64,
            Option<String>,
            Option<i64>,
            i64,
            i64,
            bool,
            bool,
            Option<i64>,
        );
        let live: Vec<Live> = {
            let mut stmt = tx.prepare(
                "SELECT id, account_id, message_id, reply_to, sent_at, remind_at, if_no_reply,
                        state = 'due', message_row
                 FROM reminders
                 WHERE (state = 'pending' AND remind_at <= ?1) OR state = 'due'",
            )?;
            stmt.query_map([now], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            })?
            .collect::<Result<_>>()?
        };
        let mut came_due = Vec::new();
        for (id, account_id, message_id, reply_to, sent_at, remind_at, if_no_reply, due, row) in
            live
        {
            let current = synced.get(&account_id).is_some_and(|&at| at >= remind_at)
                || now >= remind_at + SYNC_GRACE_MS;
            if !due && !current {
                continue;
            }
            let sent: Option<i64> = match &message_id {
                Some(message_id) => tx
                    .query_row(
                        "SELECT id FROM messages
                         WHERE account_id = ?1 AND trim(message_id, '<>') = ?2
                         ORDER BY date LIMIT 1",
                        params![account_id, message_id],
                        |r| r.get(0),
                    )
                    .optional()?,
                None => None,
            };
            let row = sent.or(row).or(reply_to);
            let replied: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM messages
                     WHERE account_id = ?1 AND date > ?2
                       AND from_address != (SELECT email FROM accounts WHERE id = ?1)
                       AND (COALESCE(conversation, id) =
                                (SELECT COALESCE(conversation, id) FROM messages WHERE id = ?3)
                            OR (?4 IS NOT NULL AND (instr(COALESCE(in_reply_to, ''), ?4) > 0
                                                    OR instr(COALESCE(refs, ''), ?4) > 0))))",
                params![account_id, sent_at, row, message_id],
                |r| r.get(0),
            )?;
            if if_no_reply && replied {
                tx.execute("DELETE FROM reminders WHERE id = ?1", [id])?;
                continue;
            }
            tx.execute(
                "UPDATE reminders SET state = 'due', message_row = ?2 WHERE id = ?1",
                params![id, row],
            )?;
            if !due {
                came_due.push(id);
            }
        }
        tx.commit()?;
        came_due
            .into_iter()
            .filter_map(|id| self.reminder(id).transpose())
            .collect()
    }

    /// A due reminder ends when its email is deleted or blocked: it's no
    /// longer something to follow up in the Inbox.
    pub(crate) fn end_reminders_of(conn: &rusqlite::Connection, message: i64) -> Result<()> {
        conn.execute(
            "UPDATE reminders SET state = 'done'
             WHERE message_row = ?1 AND state = 'due'
               AND (SELECT state FROM messages WHERE id = ?1) IN ('deleted', 'blocked')",
            [message],
        )?;
        Ok(())
    }
}
