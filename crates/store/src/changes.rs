//! What changed in the message list since the UI last read it, so it can
//! bring its copy up to date instead of reading every message again.
//!
//! Triggers log each message whose list row (`MESSAGE_COLUMNS`) may have
//! changed, under a number that grows with every write (`Mark::seq`). They
//! are temporary, made each time the store opens like
//! `contacts::WATCH_CONTACTS`, so the log covers this connection's writes
//! only. Commits by other connections (spark-cli) move `PRAGMA
//! data_version` instead, and a reader whose mark is from before one, or
//! from another opening of the store, reads everything again.

use std::collections::HashSet;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicI64, Ordering};

use serde::{Deserialize, Serialize};

use crate::{LISTED, MESSAGE_COLUMNS, Message, Result, Store, message_row, now_ms};

/// How far a reader of the message list has got: what it read reflects
/// every write up to here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mark {
    /// The opening of the store whose log this is (`Store::log`).
    log: i64,
    /// How far that log had got.
    seq: i64,
    /// `PRAGMA data_version` then: it moves when another connection commits.
    elsewhere: i64,
}

/// The message list's changes since a `Mark`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Changes {
    /// Where to ask from next time.
    pub mark: Mark,
    /// The changed messages that are listed, as `Store::messages` has them.
    pub messages: Vec<Message>,
    /// Ids of changed messages that aren't listed now: deleted, or held in
    /// the outbox to be sent. Some may never have been listed.
    pub gone: Vec<i64>,
}

/// The tables `MESSAGE_COLUMNS` and `LISTED` read, and which messages a
/// write to each changes:
///
/// - `messages`: the message, when one of its listed columns changes (the
///   `WHEN` leaves out writes that set them to what they were).
/// - `folder_members`: the message it puts in or takes out of a folder.
/// - `muted`: every message of the conversation.
/// - `reminders`: the email a reminder comes due on, or stops being due on.
/// - `outbox`: the draft it holds back from the list while it's sent.
///
/// Rows removed by foreign keys (an account, a folder) fire these too. A
/// column added to `MESSAGE_COLUMNS` must be watched here.
pub(crate) static WATCH_MESSAGES: LazyLock<String> = LazyLock::new(|| {
    let listed = [
        "id",
        "account_id",
        "category",
        "state",
        "snoozed_until",
        "unread",
        "pinned",
        "priority",
        "screening",
        "from_name",
        "from_address",
        "subject",
        "snippet",
        "date",
        "attachments",
        "home_folder_id",
        "to_list",
        "conversation",
        "reply_to_list",
        "returned_at",
        "invite",
    ];
    let changed = listed
        .iter()
        .map(|c| format!("OLD.{c} IS NOT NEW.{c}"))
        .collect::<Vec<_>>()
        .join(" OR ");
    // A conversation's messages: those that have it as `conversation`
    // (looked up per account, as indexed), and the one it started with,
    // whose id it is, in case that has none.
    let conversation = |c: &str| {
        format!(
            "SELECT m.id AS id FROM main.accounts a
                 CROSS JOIN main.messages m ON m.account_id = a.id AND m.conversation = {c}
             UNION SELECT id FROM main.messages WHERE id = {c}"
        )
    };
    format!(
        "-- Each change, in order: `seq` grows by one with each.
         CREATE TEMP TABLE message_log (seq INTEGER PRIMARY KEY, id INTEGER NOT NULL);

         CREATE TEMP TRIGGER log_message_added AFTER INSERT ON main.messages BEGIN {} END;
         CREATE TEMP TRIGGER log_message_removed AFTER DELETE ON main.messages BEGIN {} END;
         CREATE TEMP TRIGGER log_message_changed
         AFTER UPDATE OF {} ON main.messages WHEN {changed} BEGIN {} END;

         CREATE TEMP TRIGGER log_member_added AFTER INSERT ON main.folder_members BEGIN {} END;
         CREATE TEMP TRIGGER log_member_removed AFTER DELETE ON main.folder_members BEGIN {} END;
         CREATE TEMP TRIGGER log_member_changed
         AFTER UPDATE OF folder_id, message_id ON main.folder_members BEGIN {} END;

         CREATE TEMP TRIGGER log_muted AFTER INSERT ON main.muted BEGIN {} END;
         CREATE TEMP TRIGGER log_unmuted AFTER DELETE ON main.muted BEGIN {} END;
         CREATE TEMP TRIGGER log_muted_changed AFTER UPDATE ON main.muted BEGIN {} END;

         CREATE TEMP TRIGGER log_reminder_added AFTER INSERT ON main.reminders BEGIN {} END;
         CREATE TEMP TRIGGER log_reminder_removed AFTER DELETE ON main.reminders BEGIN {} END;
         CREATE TEMP TRIGGER log_reminder_changed
         AFTER UPDATE OF id, state, remind_at, message_row ON main.reminders BEGIN {} END;

         CREATE TEMP TRIGGER log_send_queued AFTER INSERT ON main.outbox BEGIN {} END;
         CREATE TEMP TRIGGER log_send_ended AFTER DELETE ON main.outbox BEGIN {} END;
         CREATE TEMP TRIGGER log_send_changed
         AFTER UPDATE OF draft_message ON main.outbox BEGIN {} END;",
        log("NEW.id", None),
        log("OLD.id", None),
        listed.join(", "),
        log("NEW.id", Some("OLD.id")),
        log("NEW.message_id", None),
        log("OLD.message_id", None),
        log("NEW.message_id", Some("OLD.message_id")),
        log_all(&conversation("NEW.conversation")),
        log_all(&conversation("OLD.conversation")),
        log_all(&format!(
            "{} UNION {}",
            conversation("OLD.conversation"),
            conversation("NEW.conversation")
        )),
        log_some("NEW.message_row", None),
        log_some("OLD.message_row", None),
        log_some("NEW.message_row", Some("OLD.message_row")),
        log_some("NEW.draft_message", None),
        log_some("OLD.draft_message", None),
        log_some("NEW.draft_message", Some("OLD.draft_message")),
    )
});

// Trigger bodies. Each is one or two plain inserts, as a sync makes many
// writes and every statement a trigger runs costs: logging a change through
// a subquery, or deduplicating the log as it's written, cost several times
// as much. Rows get the next `seq` and can't conflict, so these hold under
// any `OR IGNORE` or `OR REPLACE` of the statement that fired them, which
// overrides a trigger's own.

/// Logs message `id`, and `old` where it isn't `id`.
fn log(id: &str, old: Option<&str>) -> String {
    let mut body = format!("INSERT INTO message_log (id) VALUES ({id});");
    if let Some(old) = old {
        body.push_str(&format!(
            "INSERT INTO message_log (id) SELECT {old} WHERE {old} IS NOT {id};"
        ));
    }
    body
}

/// `log` for ids that may be null (no message).
fn log_some(id: &str, old: Option<&str>) -> String {
    let mut body = format!("INSERT INTO message_log (id) SELECT {id} WHERE {id} IS NOT NULL;");
    if let Some(old) = old {
        body.push_str(&format!(
            "INSERT INTO message_log (id) SELECT {old} WHERE {old} IS NOT NULL AND {old} IS NOT {id};"
        ));
    }
    body
}

/// Logs the message ids `ids` selects.
fn log_all(ids: &str) -> String {
    format!("INSERT INTO message_log (id) {ids};")
}

/// A number no other opening of a store in this process has had, nor (as
/// the time it opened) one in an earlier run.
pub(crate) fn new_log() -> i64 {
    static LAST: AtomicI64 = AtomicI64::new(0);
    let now = now_ms();
    let last = LAST
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |last| {
            Some(now.max(last + 1))
        })
        .expect("always updates");
    now.max(last + 1)
}

impl Store {
    /// Every message, as `messages` lists them, and the mark to ask
    /// `message_changes` from. No mark inside a transaction: a rollback
    /// would take the log back past it.
    pub fn messages_marked(&self) -> Result<(Vec<Message>, Option<Mark>)> {
        // Read first: a commit elsewhere after it shows next time.
        let mark = self.mark()?;
        Ok((self.messages()?, mark))
    }

    /// What changed in the message list since `since`: applied to what was
    /// read then, it gives what `messages` gives now. `None` when that
    /// can't be told, and the list should be read again: `since` is from
    /// another opening of the store, another connection wrote since, or
    /// this is inside a transaction.
    pub fn message_changes(&self, since: &Mark) -> Result<Option<Changes>> {
        let Some(mark) = self.mark()? else {
            return Ok(None);
        };
        // The log before `since` may be gone (see below): it's there from
        // its first entry on.
        let first: Option<i64> = self
            .conn
            .prepare_cached("SELECT MIN(seq) FROM temp.message_log")?
            .query_row([], |r| r.get(0))?;
        if since.log != mark.log
            || since.elsewhere != mark.elsewhere
            || since.seq > mark.seq
            || first.is_some_and(|first| since.seq < first - 1)
        {
            return Ok(None);
        }
        let changed: HashSet<i64> = self
            .conn
            .prepare_cached("SELECT id FROM temp.message_log WHERE seq > ?1")?
            .query_map([since.seq], |r| r.get(0))?
            .collect::<Result<_>>()?;
        let messages: Vec<Message> = if changed.is_empty() {
            Vec::new()
        } else {
            self.conn
                .prepare_cached(&format!(
                    "SELECT {MESSAGE_COLUMNS} FROM messages
                     WHERE id IN (SELECT id FROM temp.message_log WHERE seq > ?1) AND {LISTED}
                     ORDER BY date DESC, id"
                ))?
                .query_map([since.seq], message_row)?
                .collect::<Result<_>>()?
        };
        // What came before `since` was read; the last entry stays, so `seq`
        // goes on from it.
        self.conn
            .prepare_cached(
                "DELETE FROM temp.message_log
                 WHERE seq <= ?1 AND seq < (SELECT MAX(seq) FROM temp.message_log)",
            )?
            .execute([since.seq])?;
        let listed: HashSet<i64> = messages.iter().map(|m| m.id).collect();
        let mut gone: Vec<i64> = changed.difference(&listed).copied().collect();
        gone.sort_unstable();
        Ok(Some(Changes {
            mark,
            messages,
            gone,
        }))
    }

    fn mark(&self) -> Result<Option<Mark>> {
        if !self.conn.is_autocommit() {
            return Ok(None);
        }
        Ok(Some(Mark {
            log: self.log,
            seq: self
                .conn
                .prepare_cached("SELECT COALESCE(MAX(seq), 0) FROM temp.message_log")?
                .query_row([], |r| r.get(0))?,
            elsewhere: self
                .conn
                .prepare_cached("PRAGMA data_version")?
                .query_row([], |r| r.get(0))?,
        }))
    }
}

#[cfg(test)]
mod tests;
