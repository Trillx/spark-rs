//! Local storage for spark-rs: one SQLite database per profile.
//!
//! A message row keeps two views of its state. The local one (`state`,
//! `unread`, `pinned`, plus Spark-only fields like priority and snooze) is
//! what the user sees and changes. The remote one (`remote_place`,
//! `remote_unread`, `remote_pinned`) is what the server last reported.
//! `changed_at` is non-zero while they differ because of a local change, so
//! the sync engine knows what to push, and a server change only overwrites
//! local state that has nothing waiting to be pushed.

use std::collections::HashSet;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

pub use rusqlite::Error;
pub type Result<T> = rusqlite::Result<T>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AccountKind {
    Gmail,
    Microsoft,
    Imap,
}

impl AccountKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Gmail => "gmail",
            Self::Microsoft => "microsoft",
            Self::Imap => "imap",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "gmail" => Self::Gmail,
            "microsoft" => Self::Microsoft,
            _ => Self::Imap,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Category {
    #[default]
    People,
    Notifications,
    Newsletters,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageState {
    #[default]
    Inbox,
    /// Spark's Done: archived on the server.
    Done,
    Snoozed,
    /// Set Aside: parked out of the inbox without a date.
    Aside,
    Deleted,
    /// From a blocked sender. Kept on the server; hidden here.
    Blocked,
}

impl MessageState {
    /// Where the server should keep a message in this state.
    pub fn place(self) -> Place {
        match self {
            Self::Done => Place::Archive,
            Self::Deleted => Place::Trash,
            Self::Inbox | Self::Snoozed | Self::Aside | Self::Blocked => Place::Inbox,
        }
    }
}

/// The server folder a message lives in, as far as Spark cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Inbox,
    Archive,
    Trash,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub id: i64,
    pub kind: AccountKind,
    pub email: String,
    /// The user's own name, for From.
    pub name: String,
    /// Short label for the sidebar, e.g. "Work".
    pub label: String,
    pub color: String,
    /// Backend settings (hosts, client ids), JSON owned by the mail crate.
    pub config: String,
    /// Backend sync position (UIDs, delta links), JSON owned by the mail crate.
    pub sync_state: String,
}

pub struct NewAccount<'a> {
    pub kind: AccountKind,
    pub email: &'a str,
    pub name: &'a str,
    pub label: &'a str,
    pub config: &'a str,
}

/// Colors handed to new accounts in turn.
const ACCOUNT_COLORS: [&str; 6] = [
    "#0ea5e9", "#f59e0b", "#8b5cf6", "#10b981", "#f43f5e", "#6366f1",
];

/// A message as a backend fetched it.
#[derive(Clone, Debug, Default)]
pub struct Incoming {
    pub account_id: i64,
    /// Graph's immutable id.
    pub remote_id: Option<String>,
    /// IMAP mailbox and UID.
    pub mailbox: Option<String>,
    pub uid: Option<u32>,
    pub message_id: Option<String>,
    pub thread_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    pub from_name: String,
    pub from_address: String,
    /// Display strings, e.g. `Ana Ruiz <ana@example.com>`.
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub snippet: String,
    /// When it arrived, in Unix milliseconds.
    pub date: i64,
    pub text: String,
    pub html: Option<String>,
    pub attachments: Vec<Attachment>,
    pub category: Category,
    pub unread: bool,
    pub flagged: bool,
}

/// A file attached to a stored message. Only what describes it is kept; the
/// content is fetched from the server when the user opens or forwards it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub name: String,
    /// e.g. "application/pdf"; empty when unknown.
    #[serde(default)]
    pub mime: String,
    /// Decoded size in bytes; 0 when unknown.
    #[serde(default)]
    pub size: u64,
    /// Graph's attachment id. IMAP attachments are found by position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_id: Option<String>,
}

/// A file queued with an outgoing message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxFile {
    pub name: String,
    pub mime: String,
    pub data: Vec<u8>,
}

/// Where the server keeps a stored message, for fetching it again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub account_id: i64,
    pub remote_id: Option<String>,
    pub mailbox: Option<String>,
    pub uid: Option<u32>,
    pub message_id: Option<String>,
    pub remote_place: Place,
}

/// What `add_incoming` did.
#[derive(Debug, PartialEq, Eq)]
pub enum Added {
    New(i64),
    /// Already stored; its server location and flags were refreshed.
    Known(i64),
}

/// A list row: everything but the bodies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub id: i64,
    pub account_id: i64,
    pub category: Category,
    pub state: MessageState,
    pub snoozed_until: Option<i64>,
    pub unread: bool,
    pub pinned: bool,
    pub priority: bool,
    pub screening: bool,
    pub from_name: String,
    pub from_address: String,
    pub subject: String,
    pub snippet: String,
    pub date: i64,
    pub attachment: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Content {
    pub text: String,
    pub html: Option<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub attachments: Vec<Attachment>,
}

/// Threading headers of a stored message, for replies.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Headers {
    pub message_id: Option<String>,
    pub references: Option<String>,
    pub remote_id: Option<String>,
}

/// Fields the user may change; absent fields stay as they are.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Patch {
    pub state: Option<MessageState>,
    pub snoozed_until: Option<i64>,
    pub unread: Option<bool>,
    pub pinned: Option<bool>,
    pub priority: Option<bool>,
    pub screening: Option<bool>,
}

/// A local change the server hasn't seen yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub id: i64,
    pub remote_id: Option<String>,
    pub mailbox: Option<String>,
    pub uid: Option<u32>,
    pub message_id: Option<String>,
    pub place: Place,
    pub unread: bool,
    pub pinned: bool,
    pub remote_place: Place,
    pub remote_unread: bool,
    pub remote_pinned: bool,
}

/// Where a message ended up after a push, for IMAP: the new mailbox, and the
/// UID when known.
pub struct Pushed<'a> {
    pub mailbox: Option<&'a str>,
    pub uid: Option<u32>,
}

pub struct Store {
    conn: Connection,
}

const SCHEMA: &str = "
CREATE TABLE accounts (
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL,
    email TEXT NOT NULL UNIQUE COLLATE NOCASE,
    name TEXT NOT NULL,
    label TEXT NOT NULL,
    color TEXT NOT NULL,
    config TEXT NOT NULL,
    sync_state TEXT NOT NULL DEFAULT '{}',
    position INTEGER NOT NULL
);

CREATE TABLE messages (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    remote_id TEXT,
    mailbox TEXT,
    uid INTEGER,
    message_id TEXT,
    thread_id TEXT,
    in_reply_to TEXT,
    refs TEXT,
    from_name TEXT NOT NULL,
    from_address TEXT NOT NULL COLLATE NOCASE,
    to_list TEXT NOT NULL,
    cc_list TEXT NOT NULL,
    subject TEXT NOT NULL,
    snippet TEXT NOT NULL,
    date INTEGER NOT NULL,
    text_body TEXT NOT NULL,
    html_body TEXT,
    attachments TEXT NOT NULL,
    category TEXT NOT NULL,
    state TEXT NOT NULL,
    snoozed_until INTEGER,
    unread INTEGER NOT NULL,
    pinned INTEGER NOT NULL,
    priority INTEGER NOT NULL DEFAULT 0,
    screening INTEGER NOT NULL DEFAULT 0,
    remote_place TEXT NOT NULL,
    remote_unread INTEGER NOT NULL,
    remote_pinned INTEGER NOT NULL,
    changed_at INTEGER NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX messages_remote_id ON messages(account_id, remote_id) WHERE remote_id IS NOT NULL;
CREATE UNIQUE INDEX messages_uid ON messages(account_id, mailbox, uid) WHERE uid IS NOT NULL;
CREATE INDEX messages_message_id ON messages(account_id, message_id);
CREATE INDEX messages_sender ON messages(account_id, from_address);
CREATE INDEX messages_date ON messages(date DESC);

-- Gatekeeper decisions per sender.
CREATE TABLE senders (
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    address TEXT NOT NULL COLLATE NOCASE,
    status TEXT NOT NULL,
    PRIMARY KEY (account_id, address)
);

-- Messages waiting for their send time (the undo window or Send Later).
CREATE TABLE outbox (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    draft TEXT NOT NULL,
    send_at INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'queued'
);
";

/// Version 2: files attached to queued messages, copied in when queued so
/// a later send doesn't depend on the originals.
const SCHEMA_2: &str = "
CREATE TABLE outbox_files (
    id INTEGER PRIMARY KEY,
    outbox_id INTEGER NOT NULL REFERENCES outbox(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    name TEXT NOT NULL,
    mime TEXT NOT NULL,
    data BLOB NOT NULL
);
CREATE INDEX outbox_files_outbox ON outbox_files(outbox_id, position);
";

const MESSAGE_COLUMNS: &str = "id, account_id, category, state, snoozed_until, unread, pinned, \
     priority, screening, from_name, from_address, subject, snippet, date, attachments";

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        Self::configure(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::configure(Connection::open_in_memory()?)
    }

    fn configure(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version < 1 {
            conn.execute_batch(SCHEMA)?;
            conn.pragma_update(None, "user_version", 1)?;
        }
        if version < 2 {
            conn.execute_batch(SCHEMA_2)?;
            conn.pragma_update(None, "user_version", 2)?;
        }
        Ok(Self { conn })
    }

    /// Version of the SQLite library compiled into the binary.
    pub fn sqlite_version() -> &'static str {
        rusqlite::version()
    }

    /// Whether full-text search (FTS5) is available; message search depends on it.
    pub fn has_fts5(&self) -> Result<bool> {
        self.conn.query_row(
            "SELECT sqlite_compileoption_used('ENABLE_FTS5')",
            [],
            |row| row.get(0),
        )
    }

    // ---- accounts ----

    pub fn add_account(&self, new: &NewAccount) -> Result<Account> {
        let position: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM accounts",
            [],
            |r| r.get(0),
        )?;
        let color = ACCOUNT_COLORS[position as usize % ACCOUNT_COLORS.len()];
        self.conn.execute(
            "INSERT INTO accounts (kind, email, name, label, color, config, position)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                new.kind.as_str(),
                new.email,
                new.name,
                new.label,
                color,
                new.config,
                position
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        // You never need to screen yourself.
        self.set_sender(id, new.email, Some("accepted"))?;
        Ok(self.account(id)?.expect("just inserted"))
    }

    pub fn accounts(&self) -> Result<Vec<Account>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, email, name, label, color, config, sync_state
             FROM accounts ORDER BY position",
        )?;
        stmt.query_map([], account_row)?.collect()
    }

    pub fn account(&self, id: i64) -> Result<Option<Account>> {
        self.conn
            .query_row(
                "SELECT id, kind, email, name, label, color, config, sync_state
                 FROM accounts WHERE id = ?1",
                [id],
                account_row,
            )
            .optional()
    }

    pub fn has_accounts(&self) -> Result<bool> {
        self.conn
            .query_row("SELECT EXISTS (SELECT 1 FROM accounts)", [], |r| r.get(0))
    }

    /// Removes the account and everything stored for it.
    pub fn remove_account(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM accounts WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn rename_account(&self, id: i64, label: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE accounts SET label = ?2 WHERE id = ?1",
            params![id, label],
        )?;
        Ok(())
    }

    pub fn set_account_config(&self, id: i64, config: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE accounts SET config = ?2 WHERE id = ?1",
            params![id, config],
        )?;
        Ok(())
    }

    pub fn set_sync_state(&self, id: i64, sync_state: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE accounts SET sync_state = ?2 WHERE id = ?1",
            params![id, sync_state],
        )?;
        Ok(())
    }

    // ---- incoming mail ----

    /// Stores a fetched message, or refreshes the stored copy. A message is
    /// recognized by its remote id, its mailbox and UID, or failing those its
    /// Message-ID (it may have left the inbox and come back with a new UID).
    ///
    /// With `screen_new_senders`, mail from an unknown sender waits in the
    /// Gatekeeper; without it (the first sync) the sender is accepted.
    pub fn add_incoming(&mut self, m: &Incoming, screen_new_senders: bool) -> Result<Added> {
        let tx = self.conn.transaction()?;
        let existing: Option<(i64, bool)> = {
            let by_location = if let Some(remote_id) = &m.remote_id {
                tx.query_row(
                    "SELECT id, changed_at != 0 FROM messages WHERE account_id = ?1 AND remote_id = ?2",
                    params![m.account_id, remote_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?
            } else if let (Some(mailbox), Some(uid)) = (&m.mailbox, m.uid) {
                tx.query_row(
                    "SELECT id, changed_at != 0 FROM messages
                     WHERE account_id = ?1 AND mailbox = ?2 AND uid = ?3",
                    params![m.account_id, mailbox, uid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?
            } else {
                None
            };
            match (by_location, &m.message_id) {
                (Some(found), _) => Some(found),
                (None, Some(message_id)) => tx
                    .query_row(
                        "SELECT id, changed_at != 0 FROM messages
                         WHERE account_id = ?1 AND message_id = ?2
                         ORDER BY date DESC LIMIT 1",
                        params![m.account_id, message_id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?,
                (None, None) => None,
            }
        };

        if let Some((id, dirty)) = existing {
            // Flags follow the server where the user hasn't changed them;
            // the place does if nothing local is waiting at all.
            tx.execute(
                "UPDATE messages SET
                    remote_id = COALESCE(?2, remote_id), mailbox = ?3, uid = ?4,
                    unread = CASE WHEN unread = remote_unread THEN ?5 ELSE unread END,
                    pinned = CASE WHEN pinned = remote_pinned THEN ?6 ELSE pinned END,
                    state = CASE WHEN ?7 = 0 AND state IN ('done', 'deleted') THEN 'inbox' ELSE state END,
                    remote_place = 'inbox', remote_unread = ?5, remote_pinned = ?6
                 WHERE id = ?1",
                params![id, m.remote_id, m.mailbox, m.uid, m.unread, m.flagged, dirty],
            )?;
            tx.commit()?;
            return Ok(Added::Known(id));
        }

        let sender: Option<String> = tx
            .query_row(
                "SELECT status FROM senders WHERE account_id = ?1 AND address = ?2",
                params![m.account_id, m.from_address],
                |r| r.get(0),
            )
            .optional()?;
        let (state, screening) = match sender.as_deref() {
            Some("blocked") => (MessageState::Blocked, false),
            Some(_) => (MessageState::Inbox, false),
            None if screen_new_senders && !m.from_address.is_empty() => (MessageState::Inbox, true),
            None => {
                tx.execute(
                    "INSERT OR IGNORE INTO senders (account_id, address, status) VALUES (?1, ?2, 'accepted')",
                    params![m.account_id, m.from_address],
                )?;
                (MessageState::Inbox, false)
            }
        };
        tx.execute(
            "INSERT INTO messages (
                account_id, remote_id, mailbox, uid, message_id, thread_id, in_reply_to, refs,
                from_name, from_address, to_list, cc_list, subject, snippet, date,
                text_body, html_body, attachments, category, state, unread, pinned, screening,
                remote_place, remote_unread, remote_pinned)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                     ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, 'inbox', ?21, ?22)",
            params![
                m.account_id,
                m.remote_id,
                m.mailbox,
                m.uid,
                m.message_id,
                m.thread_id,
                m.in_reply_to,
                m.references,
                m.from_name,
                m.from_address,
                json(&m.to),
                json(&m.cc),
                m.subject,
                m.snippet,
                m.date,
                m.text,
                m.html,
                json(&m.attachments),
                category_str(m.category),
                state_str(state),
                m.unread,
                m.flagged,
                screening,
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(Added::New(id))
    }

    pub fn has_remote_id(&self, account_id: i64, remote_id: &str) -> Result<bool> {
        self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM messages WHERE account_id = ?1 AND remote_id = ?2)",
            params![account_id, remote_id],
            |r| r.get(0),
        )
    }

    /// UIDs stored for an IMAP mailbox.
    pub fn uids(&self, account_id: i64, mailbox: &str) -> Result<HashSet<u32>> {
        let mut stmt = self.conn.prepare(
            "SELECT uid FROM messages WHERE account_id = ?1 AND mailbox = ?2 AND uid IS NOT NULL",
        )?;
        stmt.query_map(params![account_id, mailbox], |r| r.get(0))?
            .collect()
    }

    /// Drops the UIDs of an IMAP mailbox the server renumbered (UIDVALIDITY
    /// changed). Messages keep their state and are matched again by
    /// Message-ID when refetched.
    pub fn forget_uids(&self, account_id: i64, mailbox: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE messages SET uid = NULL WHERE account_id = ?1 AND mailbox = ?2",
            params![account_id, mailbox],
        )?;
        Ok(())
    }

    /// The server's current flags for a message in an IMAP mailbox. A local
    /// flag follows unless the user changed it and the change isn't pushed yet.
    /// (In SET, columns read their old values: `unread = remote_unread` means
    /// the user hasn't touched it.)
    pub fn observe_flags(
        &self,
        account_id: i64,
        mailbox: &str,
        uid: u32,
        unread: bool,
        flagged: bool,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE messages SET remote_unread = ?4, remote_pinned = ?5,
                unread = CASE WHEN unread = remote_unread THEN ?4 ELSE unread END,
                pinned = CASE WHEN pinned = remote_pinned THEN ?5 ELSE pinned END
             WHERE account_id = ?1 AND mailbox = ?2 AND uid = ?3",
            params![account_id, mailbox, uid, unread, flagged],
        )?;
        Ok(())
    }

    /// Messages that left the server's inbox (another client archived or
    /// deleted them). Where nothing local is pending, they become Done.
    pub fn observe_gone_uids(
        &mut self,
        account_id: i64,
        mailbox: &str,
        uids: &[u32],
    ) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut changed = 0;
        for uid in uids {
            changed += tx.execute(
                "UPDATE messages SET
                    state = CASE WHEN changed_at = 0 AND state IN ('inbox', 'snoozed', 'aside')
                                 THEN 'done' ELSE state END,
                    remote_place = 'archive', mailbox = NULL, uid = NULL
                 WHERE account_id = ?1 AND mailbox = ?2 AND uid = ?3",
                params![account_id, mailbox, uid],
            )?;
        }
        tx.commit()?;
        Ok(changed)
    }

    /// Graph's version of `observe_gone_uids`.
    pub fn observe_gone_remote(&self, account_id: i64, remote_id: &str) -> Result<usize> {
        self.conn.execute(
            "UPDATE messages SET
                state = CASE WHEN changed_at = 0 AND state IN ('inbox', 'snoozed', 'aside')
                             THEN 'done' ELSE state END,
                remote_place = CASE WHEN remote_place = 'inbox' THEN 'archive' ELSE remote_place END
             WHERE account_id = ?1 AND remote_id = ?2",
            params![account_id, remote_id],
        )
    }

    // ---- reading ----

    /// Every message, newest first, without bodies.
    pub fn messages(&self) -> Result<Vec<Message>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages ORDER BY date DESC"
        ))?;
        stmt.query_map([], message_row)?.collect()
    }

    pub fn message(&self, id: i64) -> Result<Option<Message>> {
        self.conn
            .query_row(
                &format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE id = ?1"),
                [id],
                message_row,
            )
            .optional()
    }

    pub fn content(&self, id: i64) -> Result<Option<Content>> {
        self.conn
            .query_row(
                "SELECT text_body, html_body, to_list, cc_list, attachments FROM messages WHERE id = ?1",
                [id],
                |r| {
                    Ok(Content {
                        text: r.get(0)?,
                        html: r.get(1)?,
                        to: from_json(&r.get::<_, String>(2)?),
                        cc: from_json(&r.get::<_, String>(3)?),
                        attachments: attachments_from_json(&r.get::<_, String>(4)?),
                    })
                },
            )
            .optional()
    }

    pub fn location(&self, id: i64) -> Result<Option<Location>> {
        self.conn
            .query_row(
                "SELECT account_id, remote_id, mailbox, uid, message_id, remote_place
                 FROM messages WHERE id = ?1",
                [id],
                |r| {
                    Ok(Location {
                        account_id: r.get(0)?,
                        remote_id: r.get(1)?,
                        mailbox: r.get(2)?,
                        uid: r.get(3)?,
                        message_id: r.get(4)?,
                        remote_place: parse_place(&r.get::<_, String>(5)?),
                    })
                },
            )
            .optional()
    }

    pub fn headers(&self, id: i64) -> Result<Option<Headers>> {
        self.conn
            .query_row(
                "SELECT message_id, refs, remote_id FROM messages WHERE id = ?1",
                [id],
                |r| {
                    Ok(Headers {
                        message_id: r.get(0)?,
                        references: r.get(1)?,
                        remote_id: r.get(2)?,
                    })
                },
            )
            .optional()
    }

    // ---- local changes ----

    /// Applies the user's change, including the Gatekeeper's per-sender
    /// rules: accepting or blocking one message decides for the sender.
    /// Returns `None` if the message doesn't exist.
    pub fn update(&mut self, id: i64, patch: &Patch, now: i64) -> Result<Option<Message>> {
        let Some(before) = self.message(id)? else {
            return Ok(None);
        };
        let tx = self.conn.transaction()?;
        let mut m = before.clone();
        if let Some(state) = patch.state {
            m.state = state;
            if state != MessageState::Snoozed {
                m.snoozed_until = None;
            }
        }
        if let Some(until) = patch.snoozed_until {
            m.snoozed_until = Some(until);
        }
        if let Some(unread) = patch.unread {
            m.unread = unread;
        }
        if let Some(pinned) = patch.pinned {
            m.pinned = pinned;
        }
        if let Some(priority) = patch.priority {
            m.priority = priority;
        }
        if let Some(screening) = patch.screening {
            m.screening = screening;
        }
        tx.execute(
            "UPDATE messages SET state = ?2, snoozed_until = ?3, unread = ?4, pinned = ?5,
                priority = ?6, screening = ?7,
                changed_at = CASE
                    WHEN remote_place = ?8 AND remote_unread = ?4 AND remote_pinned = ?5 THEN 0
                    WHEN state != ?2 OR unread != ?4 OR pinned != ?5 THEN ?9
                    ELSE changed_at END
             WHERE id = ?1",
            params![
                id,
                state_str(m.state),
                m.snoozed_until,
                m.unread,
                m.pinned,
                m.priority,
                m.screening,
                place_str(m.state.place()),
                now,
            ],
        )?;

        let sender = (m.account_id, m.from_address.as_str());
        if patch.screening == Some(false) && before.screening {
            set_sender_in(&tx, sender, Some("accepted"))?;
            tx.execute(
                "UPDATE messages SET screening = 0 WHERE account_id = ?1 AND from_address = ?2",
                params![sender.0, sender.1],
            )?;
        } else if patch.screening == Some(true) && !before.screening {
            set_sender_in(&tx, sender, None)?;
            tx.execute(
                "UPDATE messages SET screening = 1
                 WHERE account_id = ?1 AND from_address = ?2 AND state = 'inbox'",
                params![sender.0, sender.1],
            )?;
        }
        if m.state == MessageState::Blocked && before.state != MessageState::Blocked {
            set_sender_in(&tx, sender, Some("blocked"))?;
            tx.execute(
                "UPDATE messages SET state = 'blocked', screening = 0
                 WHERE account_id = ?1 AND from_address = ?2 AND state = 'inbox'",
                params![sender.0, sender.1],
            )?;
        } else if before.state == MessageState::Blocked && m.state != MessageState::Blocked {
            set_sender_in(
                &tx,
                sender,
                if m.screening { None } else { Some("accepted") },
            )?;
        }
        tx.commit()?;
        self.message(id)
    }

    /// Accepts a sender without a message, e.g. someone the user wrote to.
    pub fn accept_sender(&self, account_id: i64, address: &str) -> Result<()> {
        self.set_sender(account_id, address, Some("accepted"))?;
        self.conn.execute(
            "UPDATE messages SET screening = 0 WHERE account_id = ?1 AND from_address = ?2",
            params![account_id, address],
        )?;
        Ok(())
    }

    fn set_sender(&self, account_id: i64, address: &str, status: Option<&str>) -> Result<()> {
        set_sender_in(&self.conn, (account_id, address), status)
    }

    /// Snoozed messages whose time has come go back to the inbox, unread.
    pub fn wake_snoozed(&self, now: i64) -> Result<usize> {
        self.conn.execute(
            "UPDATE messages SET state = 'inbox', snoozed_until = NULL, unread = 1,
                changed_at = CASE WHEN remote_unread = 1 THEN changed_at ELSE ?1 END
             WHERE state = 'snoozed' AND snoozed_until IS NOT NULL AND snoozed_until <= ?1",
            [now],
        )
    }

    /// Local changes for an account that are at least `settle_ms` old (the
    /// undo window), oldest first.
    pub fn pending(&self, account_id: i64, now: i64, settle_ms: i64) -> Result<Vec<Pending>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, remote_id, mailbox, uid, message_id, state, unread, pinned,
                    remote_place, remote_unread, remote_pinned
             FROM messages
             WHERE account_id = ?1 AND changed_at != 0 AND changed_at <= ?2
             ORDER BY changed_at",
        )?;
        stmt.query_map(params![account_id, now - settle_ms], |r| {
            Ok(Pending {
                id: r.get(0)?,
                remote_id: r.get(1)?,
                mailbox: r.get(2)?,
                uid: r.get(3)?,
                message_id: r.get(4)?,
                place: parse_state(&r.get::<_, String>(5)?).place(),
                unread: r.get(6)?,
                pinned: r.get(7)?,
                remote_place: parse_place(&r.get::<_, String>(8)?),
                remote_unread: r.get(9)?,
                remote_pinned: r.get(10)?,
            })
        })?
        .collect()
    }

    /// Records that the server now matches `p`'s local side. If the user
    /// changed the message again meanwhile, it stays pending.
    pub fn mark_pushed(&self, p: &Pending, at: Pushed) -> Result<()> {
        self.conn.execute(
            "UPDATE messages SET remote_place = ?2, remote_unread = ?3, remote_pinned = ?4,
                mailbox = ?5, uid = ?6,
                changed_at = CASE
                    WHEN ?2 = CASE state WHEN 'done' THEN 'archive' WHEN 'deleted' THEN 'trash' ELSE 'inbox' END
                         AND unread = ?3 AND pinned = ?4 THEN 0
                    ELSE changed_at END
             WHERE id = ?1",
            params![
                p.id,
                place_str(p.place),
                p.unread,
                p.pinned,
                at.mailbox,
                at.uid
            ],
        )?;
        Ok(())
    }

    /// Gives up on pushing a change (the message is gone from the server):
    /// local state stays, and nothing is retried.
    pub fn drop_pending(&self, id: i64) -> Result<()> {
        self.conn
            .execute("UPDATE messages SET changed_at = 0 WHERE id = ?1", [id])?;
        Ok(())
    }

    // ---- outbox ----

    /// Queues a draft (JSON owned by the caller) and copies of its files to
    /// send at `send_at`.
    pub fn queue_send(
        &mut self,
        account_id: i64,
        draft: &str,
        send_at: i64,
        files: &[OutboxFile],
    ) -> Result<i64> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO outbox (account_id, draft, send_at) VALUES (?1, ?2, ?3)",
            params![account_id, draft, send_at],
        )?;
        let id = tx.last_insert_rowid();
        for (position, f) in files.iter().enumerate() {
            tx.execute(
                "INSERT INTO outbox_files (outbox_id, position, name, mime, data)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, position as i64, f.name, f.mime, f.data],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }

    /// The files queued with a draft, in the order they were attached.
    pub fn outbox_files(&self, outbox_id: i64) -> Result<Vec<OutboxFile>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, mime, data FROM outbox_files WHERE outbox_id = ?1 ORDER BY position",
        )?;
        stmt.query_map([outbox_id], |r| {
            Ok(OutboxFile {
                name: r.get(0)?,
                mime: r.get(1)?,
                data: r.get(2)?,
            })
        })?
        .collect()
    }

    /// Cancels a queued send; false once sending has started or finished.
    pub fn cancel_send(&self, id: i64) -> Result<bool> {
        Ok(self.conn.execute(
            "DELETE FROM outbox WHERE id = ?1 AND status = 'queued'",
            [id],
        )? == 1)
    }

    /// Takes the due drafts for an account, marking them as sending so they
    /// can no longer be cancelled.
    pub fn claim_due_sends(&mut self, account_id: i64, now: i64) -> Result<Vec<(i64, String)>> {
        let tx = self.conn.transaction()?;
        let due: Vec<(i64, String)> = {
            let mut stmt = tx.prepare(
                "SELECT id, draft FROM outbox
                 WHERE account_id = ?1 AND status = 'queued' AND send_at <= ?2 ORDER BY send_at",
            )?;
            stmt.query_map(params![account_id, now], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_>>()?
        };
        for (id, _) in &due {
            tx.execute("UPDATE outbox SET status = 'sending' WHERE id = ?1", [id])?;
        }
        tx.commit()?;
        Ok(due)
    }

    /// Puts a claimed draft back in the queue to try again at `send_at`.
    pub fn retry_send(&self, id: i64, send_at: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE outbox SET status = 'queued', send_at = ?2 WHERE id = ?1",
            params![id, send_at],
        )?;
        Ok(())
    }

    /// The draft is out (or failed for good): forget it.
    pub fn finish_send(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM outbox WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Puts drafts that were mid-send when the app stopped back in the queue.
    pub fn requeue_interrupted_sends(&self) -> Result<usize> {
        self.conn.execute(
            "UPDATE outbox SET status = 'queued' WHERE status = 'sending'",
            [],
        )
    }
}

fn set_sender_in(
    conn: &Connection,
    (account_id, address): (i64, &str),
    status: Option<&str>,
) -> Result<()> {
    match status {
        Some(status) => conn.execute(
            "INSERT INTO senders (account_id, address, status) VALUES (?1, ?2, ?3)
             ON CONFLICT (account_id, address) DO UPDATE SET status = excluded.status",
            params![account_id, address, status],
        )?,
        None => conn.execute(
            "DELETE FROM senders WHERE account_id = ?1 AND address = ?2",
            params![account_id, address],
        )?,
    };
    Ok(())
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn account_row(r: &Row) -> Result<Account> {
    Ok(Account {
        id: r.get(0)?,
        kind: AccountKind::parse(&r.get::<_, String>(1)?),
        email: r.get(2)?,
        name: r.get(3)?,
        label: r.get(4)?,
        color: r.get(5)?,
        config: r.get(6)?,
        sync_state: r.get(7)?,
    })
}

fn message_row(r: &Row) -> Result<Message> {
    let attachments = attachments_from_json(&r.get::<_, String>(14)?);
    Ok(Message {
        id: r.get(0)?,
        account_id: r.get(1)?,
        category: parse_category(&r.get::<_, String>(2)?),
        state: parse_state(&r.get::<_, String>(3)?),
        snoozed_until: r.get(4)?,
        unread: r.get(5)?,
        pinned: r.get(6)?,
        priority: r.get(7)?,
        screening: r.get(8)?,
        from_name: r.get(9)?,
        from_address: r.get(10)?,
        subject: r.get(11)?,
        snippet: r.get(12)?,
        date: r.get(13)?,
        attachment: attachments.into_iter().next().map(|a| a.name),
    })
}

fn json<T: Serialize + ?Sized>(value: &T) -> String {
    serde_json::to_string(value).expect("plain data serializes")
}

fn from_json(text: &str) -> Vec<String> {
    serde_json::from_str(text).unwrap_or_default()
}

/// Attachments as stored: objects, or bare names in rows from before sizes
/// and types were kept.
fn attachments_from_json(text: &str) -> Vec<Attachment> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Stored {
        Name(String),
        Full(Attachment),
    }
    let stored: Vec<Stored> = serde_json::from_str(text).unwrap_or_default();
    stored
        .into_iter()
        .map(|s| match s {
            Stored::Name(name) => Attachment {
                name,
                ..Default::default()
            },
            Stored::Full(a) => a,
        })
        .collect()
}

fn category_str(c: Category) -> &'static str {
    match c {
        Category::People => "people",
        Category::Notifications => "notifications",
        Category::Newsletters => "newsletters",
    }
}

fn parse_category(s: &str) -> Category {
    match s {
        "notifications" => Category::Notifications,
        "newsletters" => Category::Newsletters,
        _ => Category::People,
    }
}

fn state_str(s: MessageState) -> &'static str {
    match s {
        MessageState::Inbox => "inbox",
        MessageState::Done => "done",
        MessageState::Snoozed => "snoozed",
        MessageState::Aside => "aside",
        MessageState::Deleted => "deleted",
        MessageState::Blocked => "blocked",
    }
}

fn parse_state(s: &str) -> MessageState {
    match s {
        "done" => MessageState::Done,
        "snoozed" => MessageState::Snoozed,
        "aside" => MessageState::Aside,
        "deleted" => MessageState::Deleted,
        "blocked" => MessageState::Blocked,
        _ => MessageState::Inbox,
    }
}

fn place_str(p: Place) -> &'static str {
    match p {
        Place::Inbox => "inbox",
        Place::Archive => "archive",
        Place::Trash => "trash",
    }
}

fn parse_place(s: &str) -> Place {
    match s {
        "archive" => Place::Archive,
        "trash" => Place::Trash,
        _ => Place::Inbox,
    }
}

#[cfg(test)]
mod tests;
