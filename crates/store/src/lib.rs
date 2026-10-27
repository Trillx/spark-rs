//! Local storage for spark-rs: one SQLite database per profile.
//!
//! A message row keeps two views of its state. The local one (`state`,
//! `unread`, `pinned`, plus Spark-only fields like priority and snooze) is
//! what the user sees and changes. The remote one (`remote_place`,
//! `remote_unread`, `remote_pinned`) is what the server last reported.
//! `changed_at` is non-zero while they differ because of a local change, so
//! the sync engine knows what to push, and a server change only overwrites
//! local state that has nothing waiting to be pushed.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

mod calendar;
mod contacts;
mod conversations;
mod drafts;
mod gatekeeper;
mod priority;
mod search;
mod signatures;

pub use calendar::{Calendar, RemoteCalendar, RemoteEvent, StoredEvent};
pub use contacts::Contact;
pub use drafts::{DraftCopy, DraftRemoval, DraftSave, DraftUpload};
pub use gatekeeper::{
    Change, Decision, GatekeeperMode, Screen, SenderStatus, domain_of, is_public_domain,
};
pub use priority::PriorityUndo;
pub use rusqlite::Error;
pub use search::{Query, Term};
pub use signatures::{DefaultSignature, Signature};
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
    /// From a blocked sender, in the account's Blocked folder.
    Blocked,
    /// In another server folder (Sent, Drafts, Spam, or one of the user's
    /// folders or labels); shown through that folder.
    Filed,
}

impl MessageState {
    /// Where the server should keep a message in this state.
    pub fn place(self) -> Place {
        match self {
            Self::Done => Place::Archive,
            Self::Deleted => Place::Trash,
            Self::Filed => Place::Folder,
            Self::Blocked => Place::Blocked,
            Self::Inbox | Self::Snoozed | Self::Aside => Place::Inbox,
        }
    }
}

/// The server folder a message lives in, as far as Spark cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Inbox,
    Archive,
    Trash,
    /// The account's Blocked folder, made when first needed.
    Blocked,
    /// Wherever the message's folder is; nothing to move.
    Folder,
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
    /// Reply-To recipients; None when a partial sync did not fetch headers.
    pub reply_to: Option<Vec<String>>,
    /// Display strings, e.g. `Ana Ruiz <ana@example.com>`.
    pub to: Vec<String>,
    pub cc: Vec<String>,
    /// Only drafts and your own sent mail carry one.
    pub bcc: Vec<String>,
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
    /// The calendar invite it carries, JSON owned by the mail crate.
    pub invite: Option<String>,
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
    /// Reply-To display entries; an empty list means replies use From.
    pub reply_to: Vec<String>,
    pub subject: String,
    pub snippet: String,
    pub date: i64,
    pub attachment: Option<String>,
    /// The stored folders that hold it, by id.
    pub folders: Vec<i64>,
    /// While Filed, the folder it's kept in (its home), if known.
    pub folder: Option<i64>,
    /// Who it's to, as display strings; Sent and Drafts list them.
    pub to: Vec<String>,
    /// The conversation it's part of (see `conversations`): messages that
    /// share it show together in the thread view.
    pub conversation: i64,
    /// When it last came back from Snooze (Unix ms): the Inbox puts it
    /// there, at the top, rather than at its date.
    pub returned_at: Option<i64>,
    /// Its conversation is muted: new mail in it doesn't notify.
    pub muted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Content {
    pub reply_to: Vec<String>,
    pub text: String,
    pub html: Option<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub attachments: Vec<Attachment>,
    /// The calendar invite it carries (JSON owned by the mail crate), and
    /// the answer to it sent from here.
    pub invite: Option<String>,
    pub rsvp: Option<String>,
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
    /// Files it into this folder of its own account: Filed there, or Done
    /// or Deleted for the Archive or Trash folder. Takes precedence over
    /// `state`.
    pub folder: Option<i64>,
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
    /// For filed mail, its home folder: the stored id, and the server's
    /// (IMAP mailbox or Graph folder id). `None` leaves it where it is.
    pub folder: Option<(i64, String)>,
    pub unread: bool,
    pub pinned: bool,
    pub remote_place: Place,
    /// The folder the server has it in, when `remote_place` is `Folder`.
    pub remote_folder: Option<i64>,
    pub remote_unread: bool,
    pub remote_pinned: bool,
}

/// Where a message ended up after a push, for IMAP: the new mailbox, and the
/// UID when known.
pub struct Pushed<'a> {
    pub mailbox: Option<&'a str>,
    pub uid: Option<u32>,
}

/// What a server folder is for. Folders without a role are the user's own
/// folders or labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FolderRole {
    Sent,
    Drafts,
    /// Where Done moves mail (Gmail: All Mail).
    Archive,
    Trash,
    Spam,
    /// Gmail's Starred, which is Pins here.
    Flagged,
    /// Where mail from blocked senders goes, as in Spark.
    Blocked,
}

impl FolderRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::Sent => "sent",
            Self::Drafts => "drafts",
            Self::Archive => "archive",
            Self::Trash => "trash",
            Self::Spam => "spam",
            Self::Flagged => "flagged",
            Self::Blocked => "blocked",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "sent" => Self::Sent,
            "drafts" => Self::Drafts,
            "archive" => Self::Archive,
            "trash" => Self::Trash,
            "spam" => Self::Spam,
            "flagged" => Self::Flagged,
            "blocked" => Self::Blocked,
            _ => return None,
        })
    }

    /// The state and server place of mail found only in such a folder.
    fn placement(role: Option<Self>) -> (MessageState, Place) {
        match role {
            Some(Self::Archive) => (MessageState::Done, Place::Archive),
            Some(Self::Trash) => (MessageState::Deleted, Place::Trash),
            Some(Self::Blocked) => (MessageState::Blocked, Place::Blocked),
            _ => (MessageState::Filed, Place::Folder),
        }
    }
}

/// A folder as a backend listed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteFolder {
    /// The IMAP mailbox name or Graph folder id.
    pub remote_id: String,
    /// The parent's `remote_id`, if the folder is nested.
    pub parent: Option<String>,
    /// Display name, without the parents.
    pub name: String,
    pub role: Option<FolderRole>,
    /// False for IMAP folders that only hold other folders.
    pub selectable: bool,
}

/// A stored server folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folder {
    pub id: i64,
    pub account_id: i64,
    pub remote_id: String,
    pub parent_id: Option<i64>,
    pub name: String,
    pub role: Option<FolderRole>,
    pub selectable: bool,
    /// Backend sync position, JSON owned by the mail crate.
    pub sync_state: String,
    /// When its messages were last synced (Unix ms); `None` until the first time.
    pub synced_at: Option<i64>,
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

-- Gatekeeper decisions per sender (until version 14, see `gatekeeper`).
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

/// Version 3: the accounts' server folders, and which stored messages each
/// folder holds (its newest ones). A message can be in several folders
/// (Gmail labels); `messages.mailbox`/`uid` stay its one location for moves.
const SCHEMA_3: &str = "
CREATE TABLE folders (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    remote_id TEXT NOT NULL,
    parent_remote TEXT,
    name TEXT NOT NULL,
    role TEXT,
    selectable INTEGER NOT NULL DEFAULT 1,
    position INTEGER NOT NULL,
    sync_state TEXT NOT NULL DEFAULT '{}',
    synced_at INTEGER,
    UNIQUE (account_id, remote_id)
);

CREATE TABLE folder_members (
    folder_id INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    uid INTEGER,
    PRIMARY KEY (folder_id, message_id)
);
CREATE INDEX folder_members_message ON folder_members(message_id);

-- Stored by a folder sync and not yet seen in the inbox: if the inbox has
-- it later, it arrives as new mail (Gatekeeper, blocked senders, notifications).
ALTER TABLE messages ADD COLUMN folder_only INTEGER NOT NULL DEFAULT 0;
";

/// Version 4: which folder filed mail lives in, locally and on the server,
/// like `state` and `remote_place`.
const SCHEMA_4: &str = "
-- The folder a Filed message is kept in. It stays when the message is
-- marked Done or deleted, so undoing that puts it back.
ALTER TABLE messages ADD COLUMN home_folder_id INTEGER REFERENCES folders(id) ON DELETE SET NULL;
-- The folder the server has it in, while remote_place is 'folder'.
ALTER TABLE messages ADD COLUMN remote_folder_id INTEGER REFERENCES folders(id) ON DELETE SET NULL;

-- Mail filed before: in its IMAP mailbox, else the first folder that lists it.
UPDATE messages SET remote_folder_id = COALESCE(
        (SELECT f.id FROM folders f
         WHERE f.account_id = messages.account_id AND f.remote_id = messages.mailbox),
        (SELECT fm.folder_id FROM folder_members fm JOIN folders f ON f.id = fm.folder_id
         WHERE fm.message_id = messages.id ORDER BY f.role IS NOT NULL, f.position LIMIT 1))
 WHERE remote_place = 'folder';
UPDATE messages SET home_folder_id = remote_folder_id WHERE state = 'filed';
";

/// Version 7: the calendar invite an email carries, and the answer the
/// user sent to it from here.
const SCHEMA_7: &str = "
ALTER TABLE messages ADD COLUMN invite TEXT;
ALTER TABLE messages ADD COLUMN rsvp TEXT;
";

/// A message a pending local change takes out of folder `?1` (its server
/// folder): until the change is pushed, syncs of that folder don't list it.
const LEAVING: &str = "changed_at != 0 AND remote_place = 'folder' AND remote_folder_id = ?1
     AND NOT (state = 'filed' AND home_folder_id IS ?1)";

/// A message a pending local change files into folder `?1`: until the
/// change is pushed, syncs of that folder keep listing it.
const ARRIVING: &str = "changed_at != 0 AND state = 'filed' AND home_folder_id = ?1";

/// A message's local and server sides, for `Pending`; see `pending_row`.
const PENDING_SELECT: &str = "SELECT m.id, m.remote_id, m.mailbox, m.uid, m.message_id, m.state,
        m.unread, m.pinned, m.remote_place, m.remote_unread, m.remote_pinned,
        m.home_folder_id, f.remote_id, m.remote_folder_id
     FROM messages m LEFT JOIN folders f ON f.id = m.home_folder_id";

fn pending_row(r: &Row) -> Result<Pending> {
    let place = parse_state(&r.get::<_, String>(5)?).place();
    let home: (Option<i64>, Option<String>) = (r.get(11)?, r.get(12)?);
    Ok(Pending {
        id: r.get(0)?,
        remote_id: r.get(1)?,
        mailbox: r.get(2)?,
        uid: r.get(3)?,
        message_id: r.get(4)?,
        place,
        folder: match home {
            (Some(id), Some(remote_id)) if place == Place::Folder => Some((id, remote_id)),
            _ => None,
        },
        unread: r.get(6)?,
        pinned: r.get(7)?,
        remote_place: parse_place(&r.get::<_, String>(8)?),
        remote_folder: r.get(13)?,
        remote_unread: r.get(9)?,
        remote_pinned: r.get(10)?,
    })
}

const MESSAGE_COLUMNS: &str = "id, account_id, category, state, snoozed_until, unread, pinned, \
     priority, screening, from_name, from_address, subject, snippet, date, attachments, \
     (SELECT group_concat(folder_id) FROM folder_members WHERE message_id = messages.id), \
     CASE WHEN state = 'filed' THEN home_folder_id END, to_list, COALESCE(conversation, id), reply_to_list, \
     returned_at, EXISTS (SELECT 1 FROM muted WHERE muted.conversation = COALESCE(messages.conversation, messages.id))";

const FOLDER_COLUMNS: &str = "f.id, f.account_id, f.remote_id, p.id, f.name, f.role, f.selectable, \
     f.sync_state, f.synced_at";

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
        if version < 3 {
            conn.execute_batch(SCHEMA_3)?;
            conn.pragma_update(None, "user_version", 3)?;
        }
        if version < 4 {
            conn.execute_batch(SCHEMA_4)?;
            conn.pragma_update(None, "user_version", 4)?;
        }
        if version < 5 {
            conn.execute_batch(drafts::SCHEMA_DRAFTS)?;
            conn.pragma_update(None, "user_version", 5)?;
        }
        if version < 6 {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(conversations::SCHEMA_CONVERSATIONS)?;
            conversations::backfill(&tx)?;
            tx.pragma_update(None, "user_version", 6)?;
            tx.commit()?;
        }
        if version < 7 {
            conn.execute_batch(SCHEMA_7)?;
            conn.pragma_update(None, "user_version", 7)?;
        }
        if version < 8 {
            conn.execute_batch(calendar::SCHEMA_CALENDAR)?;
            conn.pragma_update(None, "user_version", 8)?;
        }
        if version < 9 {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(search::SCHEMA_SEARCH)?;
            tx.pragma_update(None, "user_version", 9)?;
            tx.commit()?;
        }
        if version < 10 {
            conn.execute_batch(signatures::SCHEMA_SIGNATURES)?;
            conn.pragma_update(None, "user_version", 10)?;
        }
        if version < 11 {
            conn.execute_batch(contacts::SCHEMA_CONTACTS)?;
            conn.pragma_update(None, "user_version", 11)?;
        }
        if version < 12 {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(
                "ALTER TABLE messages ADD COLUMN reply_to_list TEXT NOT NULL DEFAULT '[]';
                 CREATE TABLE draft_options (
                    message_id INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
                    send_later TEXT NOT NULL
                 );",
            )?;
            tx.pragma_update(None, "user_version", 12)?;
            tx.commit()?;
        }
        if version < 13 {
            // Blocked mail used to stay in the server's inbox: move it out.
            conn.execute(
                "UPDATE messages SET changed_at = ?1 WHERE state = 'blocked' AND remote_place != 'blocked'",
                [now_ms()],
            )?;
            conn.pragma_update(None, "user_version", 13)?;
        }
        if version < 14 {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(gatekeeper::SCHEMA_GATEKEEPER)?;
            gatekeeper::migrate(&tx, now_ms())?;
            tx.pragma_update(None, "user_version", 14)?;
            tx.commit()?;
        }
        if version < 15 {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(
                "ALTER TABLE messages ADD COLUMN returned_at INTEGER;
                 -- Muted conversations, by id (`Message::conversation`).
                 CREATE TABLE muted (conversation INTEGER PRIMARY KEY);",
            )?;
            tx.pragma_update(None, "user_version", 15)?;
            tx.commit()?;
        }
        if version < 16 {
            conn.execute_batch(priority::SCHEMA_PRIORITY)?;
            conn.pragma_update(None, "user_version", 16)?;
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
        gatekeeper::welcome_colleagues(&self.conn, new.email, now_ms())?;
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
    /// With `screen_new_senders`, mail from a new sender waits in the
    /// Gatekeeper (see `gatekeeper::arrival`); without it (the first sync)
    /// the sender is accepted.
    pub fn add_incoming(&mut self, m: &Incoming, screen_new_senders: bool) -> Result<Added> {
        let tx = self.conn.transaction()?;
        if let Some(id) = find_stored(&tx, m)? {
            refresh_reply_to(&tx, id, m)?;
            let (dirty, folder_only): (bool, bool) = tx.query_row(
                "SELECT changed_at != 0, folder_only FROM messages WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            // First seen by a folder sync (Gmail's All Mail has inbox mail
            // too): it arrives now, as if it were new.
            if folder_only && !dirty {
                let (state, screening) = gatekeeper::arrival(&tx, m, screen_new_senders)?;
                tx.execute(
                    "UPDATE messages SET
                        remote_id = COALESCE(?2, remote_id), mailbox = ?3, uid = ?4,
                        unread = ?5, pinned = ?6, state = ?7, screening = ?8, folder_only = 0,
                        remote_place = 'inbox', remote_folder_id = NULL,
                        remote_unread = ?5, remote_pinned = ?6, changed_at = ?9
                     WHERE id = ?1",
                    params![
                        id,
                        m.remote_id,
                        m.mailbox,
                        m.uid,
                        m.unread,
                        m.flagged,
                        state_str(state),
                        screening,
                        arrival_change(state)
                    ],
                )?;
                tx.commit()?;
                return Ok(Added::New(id));
            }
            // Flags follow the server where the user hasn't changed them;
            // the place does if nothing local is waiting at all (mail
            // moved back to the inbox elsewhere, even out of Blocked).
            tx.execute(
                "UPDATE messages SET
                    remote_id = COALESCE(?2, remote_id), mailbox = ?3, uid = ?4,
                    unread = CASE WHEN unread = remote_unread THEN ?5 ELSE unread END,
                    pinned = CASE WHEN pinned = remote_pinned THEN ?6 ELSE pinned END,
                    state = CASE WHEN ?7 = 0 AND state IN ('done', 'deleted', 'filed', 'blocked') THEN 'inbox' ELSE state END,
                    remote_place = 'inbox', remote_folder_id = NULL,
                    remote_unread = ?5, remote_pinned = ?6, folder_only = 0
                 WHERE id = ?1",
                params![id, m.remote_id, m.mailbox, m.uid, m.unread, m.flagged, dirty],
            )?;
            tx.commit()?;
            return Ok(Added::Known(id));
        }

        let (state, screening) = gatekeeper::arrival(&tx, m, screen_new_senders)?;
        let id = insert_message(&tx, m, state, screening, Place::Inbox)?;
        tx.execute(
            "UPDATE messages SET changed_at = ?2 WHERE id = ?1",
            params![id, arrival_change(state)],
        )?;
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

    /// Header metadata for a message already stored. Flags-only updates
    /// (`reply_to: None`) leave the addresses alone. Returns whether it changed.
    pub fn observe_reply_to(&self, m: &Incoming) -> Result<bool> {
        match find_stored(&self.conn, m)? {
            Some(id) => refresh_reply_to(&self.conn, id, m),
            None => Ok(false),
        }
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

    /// Every message, newest first, without bodies. Drafts waiting in the
    /// outbox to be sent aren't listed.
    pub fn messages(&self) -> Result<Vec<Message>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages
             WHERE id NOT IN (SELECT draft_message FROM outbox WHERE draft_message IS NOT NULL)
             ORDER BY date DESC"
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
                "SELECT text_body, html_body, to_list, cc_list, attachments, bcc_list, invite, rsvp, reply_to_list
                 FROM messages WHERE id = ?1",
                [id],
                |r| {
                    Ok(Content {
                        reply_to: from_json(&r.get::<_, String>(8)?),
                        text: r.get(0)?,
                        html: r.get(1)?,
                        to: from_json(&r.get::<_, String>(2)?),
                        cc: from_json(&r.get::<_, String>(3)?),
                        bcc: from_json(&r.get::<_, String>(5)?),
                        attachments: attachments_from_json(&r.get::<_, String>(4)?),
                        invite: r.get(6)?,
                        rsvp: r.get(7)?,
                    })
                },
            )
            .optional()
    }

    /// Records the answer sent to the invite message `id` carries.
    pub fn set_rsvp(&self, id: i64, answer: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE messages SET rsvp = ?2 WHERE id = ?1",
            params![id, answer],
        )?;
        Ok(())
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

    /// Applies the user's change to one message; decisions about its sender
    /// are `decide`'s. Returns `None` if the message doesn't exist.
    ///
    /// Filed mail is listed in its home folder from now on, and no longer in
    /// the one it leaves; folder syncs keep it that way until the change is
    /// pushed (see `LEAVING` and `ARRIVING`).
    pub fn update(&mut self, id: i64, patch: &Patch, now: i64) -> Result<Option<Message>> {
        let Some(before) = self.message(id)? else {
            return Ok(None);
        };
        let tx = self.conn.transaction()?;
        let mut m = before.clone();
        let (mut home, remote_folder): (Option<i64>, Option<i64>) = tx.query_row(
            "SELECT home_folder_id, remote_folder_id FROM messages WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mut state = patch.state;
        if let Some(folder_id) = patch.folder {
            let filed = filing(&tx, m.account_id, folder_id)?;
            if filed == MessageState::Filed {
                home = Some(folder_id);
            }
            state = Some(filed);
        }
        if let Some(state) = state {
            m.state = state;
            if state != MessageState::Snoozed {
                m.snoozed_until = None;
            }
        }
        // Filed without a folder (an older undo): back where it was kept,
        // else wherever the server has it.
        if m.state == MessageState::Filed && home.is_none() {
            home = remote_folder;
        }
        m.folder = home.filter(|_| m.state == MessageState::Filed);
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
        // A filed message is where the server has it if that's its home
        // folder, or it has none.
        tx.execute(
            "UPDATE messages SET state = ?2, snoozed_until = ?3, unread = ?4, pinned = ?5,
                priority = ?6, screening = ?7, home_folder_id = ?10,
                changed_at = CASE
                    WHEN remote_place = ?8 AND (?8 != 'folder' OR ?10 IS NULL OR remote_folder_id = ?10)
                         AND remote_unread = ?4 AND remote_pinned = ?5 THEN 0
                    WHEN state != ?2 OR home_folder_id IS NOT ?10 OR unread != ?4 OR pinned != ?5
                         THEN ?9
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
                home,
            ],
        )?;
        if before.folder != m.folder {
            if let Some(left) = before.folder {
                tx.execute(
                    "DELETE FROM folder_members WHERE folder_id = ?1 AND message_id = ?2",
                    params![left, id],
                )?;
            }
            if let Some(home) = m.folder {
                // With its UID there if the server still has it there.
                tx.execute(
                    "INSERT INTO folder_members (folder_id, message_id, uid)
                     SELECT ?1, id, CASE WHEN mailbox = (SELECT remote_id FROM folders WHERE id = ?1)
                                         THEN uid END
                     FROM messages WHERE id = ?2
                     ON CONFLICT (folder_id, message_id) DO NOTHING",
                    params![home, id],
                )?;
            }
        }

        tx.commit()?;
        self.message(id)
    }

    /// Snoozed messages whose time has come go back to the inbox, unread,
    /// at the top (`returned_at`), or to Blocked if their sender was blocked
    /// meanwhile. Returns their ids.
    pub fn wake_snoozed(&self, now: i64) -> Result<Vec<i64>> {
        let blocked = gatekeeper::SENDER_BLOCKED;
        let mut stmt = self.conn.prepare(&format!(
            "UPDATE messages SET
                state = CASE WHEN {blocked} THEN 'blocked' ELSE 'inbox' END,
                returned_at = CASE WHEN {blocked} THEN NULL ELSE snoozed_until END,
                snoozed_until = NULL, unread = 1,
                changed_at = CASE WHEN remote_unread = 1 AND NOT {blocked} THEN changed_at ELSE ?1 END
             WHERE state = 'snoozed' AND snoozed_until IS NOT NULL AND snoozed_until <= ?1
             RETURNING id"
        ))?;
        stmt.query_map([now], |r| r.get(0))?.collect()
    }

    /// Mutes or unmutes a conversation (`Message::conversation`).
    pub fn set_muted(&self, conversation: i64, muted: bool) -> Result<()> {
        let sql = if muted {
            "INSERT OR IGNORE INTO muted (conversation) VALUES (?1)"
        } else {
            "DELETE FROM muted WHERE conversation = ?1"
        };
        self.conn.execute(sql, [conversation])?;
        Ok(())
    }

    /// Local changes for an account that are at least `settle_ms` old (the
    /// undo window), oldest first.
    pub fn pending(&self, account_id: i64, now: i64, settle_ms: i64) -> Result<Vec<Pending>> {
        let mut stmt = self.conn.prepare(&format!(
            "{PENDING_SELECT}
             WHERE m.account_id = ?1 AND m.changed_at != 0 AND m.changed_at <= ?2
             ORDER BY m.changed_at"
        ))?;
        stmt.query_map(params![account_id, now - settle_ms], pending_row)?
            .collect()
    }

    /// A message as `pending` describes it, changed or not: for finding it
    /// on the server to delete it there.
    pub fn pending_for(&self, id: i64) -> Result<Option<Pending>> {
        self.conn
            .query_row(
                &format!("{PENDING_SELECT} WHERE m.id = ?1"),
                [id],
                pending_row,
            )
            .optional()
    }

    /// Forgets messages the server deleted for good. Returns how many went.
    pub fn remove_messages(&self, ids: &[i64]) -> Result<usize> {
        let mut removed = 0;
        for id in ids {
            removed += self
                .conn
                .execute("DELETE FROM messages WHERE id = ?1", [id])?;
        }
        Ok(removed)
    }

    /// Forgets the mail the server kept in a folder that was just emptied
    /// there: IMAP mail located in its mailbox, and Graph mail filed in it
    /// (or in Deleted Items, for the trash folder). Mail changed since the
    /// folder was emptied stays for the next sync to sort out. Returns how
    /// many went.
    pub fn forget_emptied(&self, folder_id: i64) -> Result<usize> {
        let Some(folder) = self.folder(folder_id)? else {
            return Ok(0);
        };
        let trash = folder.role == Some(FolderRole::Trash);
        self.conn.execute(
            "DELETE FROM messages
             WHERE account_id = ?1 AND changed_at = 0
               AND (mailbox = ?2
                    OR (remote_place = 'folder' AND remote_folder_id = ?3)
                    OR (?4 AND remote_place = 'trash'))",
            params![folder.account_id, folder.remote_id, folder.id, trash],
        )
    }

    /// Records that the server now matches `p`'s local side. If the user
    /// changed the message again meanwhile, it stays pending, also when the
    /// change took it back to where the server had it before the push.
    pub fn mark_pushed(&self, p: &Pending, at: Pushed) -> Result<()> {
        // Filed mail is in its home folder now, or stayed where it was.
        let remote_folder = match p.place {
            Place::Folder => p.folder.as_ref().map(|(id, _)| *id).or(p.remote_folder),
            _ => None,
        };
        self.conn.execute(
            "UPDATE messages SET remote_place = ?2, remote_unread = ?3, remote_pinned = ?4,
                mailbox = ?5, uid = ?6, remote_folder_id = (SELECT id FROM folders WHERE id = ?7),
                changed_at = CASE
                    WHEN ?2 = CASE state WHEN 'done' THEN 'archive' WHEN 'deleted' THEN 'trash'
                                         WHEN 'blocked' THEN 'blocked' WHEN 'filed' THEN 'folder'
                                         ELSE 'inbox' END
                         AND (?2 != 'folder' OR home_folder_id IS NULL OR home_folder_id IS ?7)
                         AND unread = ?3 AND pinned = ?4 THEN 0
                    WHEN changed_at = 0 THEN ?8
                    ELSE changed_at END
             WHERE id = ?1",
            params![
                p.id,
                place_str(p.place),
                p.unread,
                p.pinned,
                at.mailbox,
                at.uid,
                remote_folder,
                now_ms()
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

    // ---- folders ----

    /// Makes `listed` (in the server's order) the account's folder list.
    /// Folders keep their ids, and their messages, from one listing to the
    /// next; ones the server no longer has go, with the mail only they held.
    /// Returns whether anything changed.
    pub fn replace_folders(&mut self, account_id: i64, listed: &[RemoteFolder]) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let mut changed = 0;
        for (position, f) in listed.iter().enumerate() {
            changed += tx.execute(
                "INSERT INTO folders (account_id, remote_id, parent_remote, name, role, selectable, position)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (account_id, remote_id) DO UPDATE SET
                    parent_remote = excluded.parent_remote, name = excluded.name,
                    role = excluded.role, selectable = excluded.selectable,
                    position = excluded.position
                 WHERE parent_remote IS NOT excluded.parent_remote OR name != excluded.name
                    OR role IS NOT excluded.role OR selectable != excluded.selectable
                    OR position != excluded.position",
                params![
                    account_id,
                    f.remote_id,
                    f.parent,
                    f.name,
                    f.role.map(FolderRole::as_str),
                    f.selectable,
                    position as i64
                ],
            )?;
        }
        let keep: HashSet<&str> = listed.iter().map(|f| f.remote_id.as_str()).collect();
        let stored: Vec<(i64, String)> = {
            let mut stmt = tx.prepare("SELECT id, remote_id FROM folders WHERE account_id = ?1")?;
            stmt.query_map([account_id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_>>()?
        };
        for (id, remote_id) in stored {
            if !keep.contains(remote_id.as_str()) {
                changed += forget_folder_in(&tx, id)?;
            }
        }
        tx.commit()?;
        Ok(changed > 0)
    }

    /// Every stored folder: accounts in order, each account's in the server's order.
    pub fn folders(&self) -> Result<Vec<Folder>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {FOLDER_COLUMNS} FROM folders f
             JOIN accounts a ON a.id = f.account_id
             LEFT JOIN folders p ON p.account_id = f.account_id AND p.remote_id = f.parent_remote
             ORDER BY a.position, f.position"
        ))?;
        stmt.query_map([], folder_row)?.collect()
    }

    pub fn folder(&self, id: i64) -> Result<Option<Folder>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {FOLDER_COLUMNS} FROM folders f
                     LEFT JOIN folders p ON p.account_id = f.account_id AND p.remote_id = f.parent_remote
                     WHERE f.id = ?1"
                ),
                [id],
                folder_row,
            )
            .optional()
    }

    /// Records a finished sync of a folder's messages.
    pub fn set_folder_synced(&self, folder_id: i64, sync_state: &str, at: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE folders SET sync_state = ?2, synced_at = ?3 WHERE id = ?1",
            params![folder_id, sync_state, at],
        )?;
        Ok(())
    }

    /// The folder's messages by id.
    pub fn folder_member_ids(&self, folder_id: i64) -> Result<HashSet<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT message_id FROM folder_members WHERE folder_id = ?1")?;
        stmt.query_map([folder_id], |r| r.get(0))?.collect()
    }

    /// An IMAP folder's messages: message id by UID.
    pub fn folder_uids(&self, folder_id: i64) -> Result<HashMap<u32, i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT uid, message_id FROM folder_members WHERE folder_id = ?1 AND uid IS NOT NULL",
        )?;
        stmt.query_map([folder_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect()
    }

    /// IMAP folders that hold a message, with its UID in each: where to find
    /// filed mail whose own location is unknown.
    pub fn member_locations(&self, message_id: i64) -> Result<Vec<(String, u32)>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.remote_id, fm.uid FROM folder_members fm JOIN folders f ON f.id = fm.folder_id
             WHERE fm.message_id = ?1 AND fm.uid IS NOT NULL ORDER BY f.position",
        )?;
        stmt.query_map([message_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect()
    }

    pub fn has_message_id(&self, account_id: i64, message_id: &str) -> Result<bool> {
        self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM messages WHERE account_id = ?1 AND message_id = ?2)",
            params![account_id, message_id],
            |r| r.get(0),
        )
    }

    /// Stores a message a folder sync found, or notes that a stored one is
    /// in the folder. Mail found only in folders isn't in the inbox: the
    /// Archive folder's is Done, Trash's Deleted, any other's Filed there
    /// (its home folder).
    ///
    /// A stored message only moves into the folder (taking its location,
    /// state and flags) if the server doesn't have it in the inbox, nothing
    /// local is waiting, and, for IMAP, it has no other known location: a
    /// Gmail message is in every folder of its labels, and the first one
    /// found (or the one it was moved to from here) stays its location so
    /// syncs don't toggle it between them. Mail a pending change takes out
    /// of the folder isn't listed in it.
    pub fn add_folder_message(&mut self, folder_id: i64, m: &Incoming) -> Result<Added> {
        let tx = self.conn.transaction()?;
        let role: Option<String> =
            tx.query_row("SELECT role FROM folders WHERE id = ?1", [folder_id], |r| {
                r.get(0)
            })?;
        let (state, place) = FolderRole::placement(role.as_deref().and_then(FolderRole::parse));
        let home = (place == Place::Folder).then_some(folder_id);
        let added = if let Some(id) = find_stored(&tx, m)? {
            refresh_reply_to(&tx, id, m)?;
            let (dirty, remote_place, mailbox): (bool, String, Option<String>) = tx.query_row(
                "SELECT changed_at != 0, remote_place, mailbox FROM messages WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            let claim = !dirty
                && remote_place != "inbox"
                && (m.uid.is_none() || mailbox.is_none() || mailbox == m.mailbox);
            if claim {
                if let (Some(mailbox), Some(uid)) = (&m.mailbox, m.uid) {
                    // A stale row holding this location would break the unique index.
                    tx.execute(
                        "UPDATE messages SET mailbox = NULL, uid = NULL
                         WHERE account_id = ?1 AND mailbox = ?2 AND uid = ?3 AND id != ?4",
                        params![m.account_id, mailbox, uid, id],
                    )?;
                }
                tx.execute(
                    "UPDATE messages SET remote_id = COALESCE(?2, remote_id), mailbox = ?3, uid = ?4,
                        state = ?5, unread = ?6, pinned = ?7,
                        remote_place = ?8, remote_unread = ?6, remote_pinned = ?7,
                        home_folder_id = COALESCE(?9, home_folder_id), remote_folder_id = ?9
                     WHERE id = ?1",
                    params![
                        id,
                        m.remote_id,
                        m.mailbox,
                        m.uid,
                        state_str(state),
                        m.unread,
                        m.flagged,
                        place_str(place),
                        home
                    ],
                )?;
            }
            Added::Known(id)
        } else {
            let id = insert_message(&tx, m, state, false, place)?;
            tx.execute(
                "UPDATE messages SET folder_only = 1, home_folder_id = ?2, remote_folder_id = ?2
                 WHERE id = ?1",
                params![id, home],
            )?;
            Added::New(id)
        };
        let id = match added {
            Added::New(id) | Added::Known(id) => id,
        };
        add_member(&tx, folder_id, id, m.uid)?;
        if role.as_deref() == Some(FolderRole::Sent.as_str()) {
            gatekeeper::sent(&tx, id)?;
        }
        tx.commit()?;
        Ok(added)
    }

    /// Makes `members` (message ids, with IMAP UIDs) the folder's whole
    /// membership, except for mail a pending change files into the folder
    /// or takes out of it, and drafts saved here and not uploaded yet (the
    /// server can't list them). Returns how many messages left it.
    pub fn set_folder_members(
        &mut self,
        folder_id: i64,
        members: &[(i64, Option<u32>)],
    ) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let wanted: HashSet<i64> = members.iter().map(|(id, _)| *id).collect();
        let before: Vec<i64> = {
            let mut stmt = tx.prepare(
                "SELECT message_id FROM folder_members WHERE folder_id = ?1
                 AND message_id NOT IN (SELECT message_id FROM draft_uploads)",
            )?;
            stmt.query_map([folder_id], |r| r.get(0))?
                .collect::<Result<_>>()?
        };
        let mut left = 0;
        for id in before.iter().filter(|id| !wanted.contains(id)) {
            left += tx.execute(
                &format!(
                    "DELETE FROM folder_members WHERE folder_id = ?1 AND message_id = ?2
                       AND NOT EXISTS (SELECT 1 FROM messages WHERE id = ?2 AND {ARRIVING})"
                ),
                params![folder_id, id],
            )?;
        }
        for (id, uid) in members {
            add_member(&tx, folder_id, *id, *uid)?;
        }
        tx.commit()?;
        Ok(left)
    }

    /// Graph's current flags for a stored message, followed where the user
    /// hasn't changed them (as in `observe_flags`).
    pub fn observe_remote_flags(
        &self,
        account_id: i64,
        remote_id: &str,
        unread: bool,
        flagged: bool,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE messages SET remote_unread = ?3, remote_pinned = ?4,
                unread = CASE WHEN unread = remote_unread THEN ?3 ELSE unread END,
                pinned = CASE WHEN pinned = remote_pinned THEN ?4 ELSE pinned END
             WHERE account_id = ?1 AND remote_id = ?2",
            params![account_id, remote_id, unread, flagged],
        )?;
        Ok(())
    }

    /// The server renumbered an IMAP folder (UIDVALIDITY changed): its
    /// membership and the UIDs of messages located there are forgotten, to
    /// be found again by Message-ID.
    pub fn forget_folder_uids(&self, folder_id: i64) -> Result<()> {
        let Some(folder) = self.folder(folder_id)? else {
            return Ok(());
        };
        self.conn.execute(
            "DELETE FROM folder_members WHERE folder_id = ?1",
            [folder_id],
        )?;
        self.forget_uids(folder.account_id, &folder.remote_id)
    }

    /// A folder was renamed on the server. An IMAP rename also renames the
    /// folders nested in it (their names start with the old name), and
    /// moves the location of messages in them; Graph ids don't change.
    pub fn rename_folder(&mut self, folder_id: i64, remote_id: &str, name: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        let (account_id, old): (i64, String) = tx.query_row(
            "SELECT account_id, remote_id FROM folders WHERE id = ?1",
            [folder_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        tx.execute(
            "UPDATE folders SET name = ?2 WHERE id = ?1",
            params![folder_id, name],
        )?;
        if old != remote_id {
            let subtree = folder_subtree(&tx, folder_id)?;
            let renamed = |r: &str| match r.strip_prefix(old.as_str()) {
                Some(rest) => format!("{remote_id}{rest}"),
                None => r.to_owned(),
            };
            let inside: HashSet<&str> = subtree.iter().map(|f| f.1.as_str()).collect();
            for (id, r, parent) in &subtree {
                let parent = parent.as_deref().map(|p| {
                    if inside.contains(p) {
                        renamed(p)
                    } else {
                        p.to_owned()
                    }
                });
                tx.execute(
                    "UPDATE folders SET remote_id = ?2, parent_remote = ?3 WHERE id = ?1",
                    params![id, renamed(r), parent],
                )?;
                tx.execute(
                    "UPDATE messages SET mailbox = ?3 WHERE account_id = ?1 AND mailbox = ?2",
                    params![account_id, r, renamed(r)],
                )?;
            }
        }
        tx.commit()
    }

    /// A folder was deleted on the server, with any nested in it: they go,
    /// with the mail only they held.
    pub fn remove_folder(&mut self, folder_id: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        for (id, ..) in folder_subtree(&tx, folder_id)?.into_iter().rev() {
            forget_folder_in(&tx, id)?;
        }
        tx.commit()
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

/// The stored copy of a fetched message: by its Graph id, which never
/// changes (a copy in Sent Items has its own); else by IMAP mailbox and UID,
/// or failing those its Message-ID (it may have moved and come back with a
/// new UID).
fn find_stored(conn: &Connection, m: &Incoming) -> Result<Option<i64>> {
    if let Some(remote_id) = &m.remote_id {
        return conn
            .query_row(
                "SELECT id FROM messages WHERE account_id = ?1 AND remote_id = ?2",
                params![m.account_id, remote_id],
                |r| r.get(0),
            )
            .optional();
    }
    let by_location = if let (Some(mailbox), Some(uid)) = (&m.mailbox, m.uid) {
        conn.query_row(
            "SELECT id FROM messages WHERE account_id = ?1 AND mailbox = ?2 AND uid = ?3",
            params![m.account_id, mailbox, uid],
            |r| r.get(0),
        )
        .optional()?
    } else {
        None
    };
    match (by_location, &m.message_id) {
        (Some(id), _) => Ok(Some(id)),
        (None, Some(message_id)) => conn
            .query_row(
                "SELECT id FROM messages WHERE account_id = ?1 AND message_id = ?2
                 ORDER BY date DESC LIMIT 1",
                params![m.account_id, message_id],
                |r| r.get(0),
            )
            .optional(),
        (None, None) => Ok(None),
    }
}

/// Lists a message in a folder, with its UID there, unless a pending change
/// takes it out of that folder.
fn add_member(conn: &Connection, folder_id: i64, message_id: i64, uid: Option<u32>) -> Result<()> {
    conn.execute(
        &format!(
            "INSERT INTO folder_members (folder_id, message_id, uid)
             SELECT ?1, id, ?3 FROM messages WHERE id = ?2 AND NOT ({LEAVING})
             ON CONFLICT (folder_id, message_id) DO UPDATE SET uid = excluded.uid"
        ),
        params![folder_id, message_id, uid],
    )?;
    Ok(())
}

/// What filing a message of `account_id` into a folder makes it: Done for
/// the Archive folder, Deleted for Trash, else Filed. An error worded for
/// the user if it can't go there.
fn filing(conn: &Connection, account_id: i64, folder_id: i64) -> Result<MessageState> {
    let folder: Option<(i64, Option<String>, bool)> = conn
        .query_row(
            "SELECT account_id, role, selectable FROM folders WHERE id = ?1",
            [folder_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((owner, role, selectable)) = folder else {
        return Err(refused("That folder is gone."));
    };
    if owner != account_id {
        return Err(refused(
            "An email can only be moved to a folder of its own account.",
        ));
    }
    if !selectable {
        return Err(refused("That folder only holds other folders."));
    }
    Ok(FolderRole::placement(role.as_deref().and_then(FolderRole::parse)).0)
}

/// A change the store won't make, as a constraint error worded for the user.
fn refused(why: &str) -> Error {
    Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
        Some(why.to_owned()),
    )
}

/// `changed_at` for mail arriving in the inbox as `state`: mail from a
/// blocked sender has to move to the Blocked folder.
fn arrival_change(state: MessageState) -> i64 {
    if state == MessageState::Blocked {
        now_ms()
    } else {
        0
    }
}

/// Full header fetches refresh reply addresses; flags-only fetches leave them alone.
fn refresh_reply_to(conn: &Connection, id: i64, m: &Incoming) -> Result<bool> {
    let Some(recipients) = &m.reply_to else {
        return Ok(false);
    };
    Ok(conn.execute(
        "UPDATE messages SET reply_to_list = ?2 WHERE id = ?1 AND reply_to_list != ?2",
        params![id, json(recipients)],
    )? > 0)
}

fn insert_message(
    conn: &Connection,
    m: &Incoming,
    state: MessageState,
    screening: bool,
    place: Place,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO messages (
            account_id, remote_id, mailbox, uid, message_id, thread_id, in_reply_to, refs,
            from_name, from_address, to_list, cc_list, subject, snippet, date,
            text_body, html_body, attachments, category, state, unread, pinned, screening,
            remote_place, remote_unread, remote_pinned, bcc_list, invite, reply_to_list)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                 ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?21, ?22, ?25, ?26, ?27)",
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
            place_str(place),
            json(&m.bcc),
            m.invite,
            json(m.reply_to.as_deref().unwrap_or_default()),
        ],
    )?;
    let id = conn.last_insert_rowid();
    let threading = conversations::Threading {
        message_id: m.message_id.as_deref(),
        in_reply_to: m.in_reply_to.as_deref(),
        references: m.references.as_deref(),
        thread_id: m.thread_id.as_deref(),
        subject: &m.subject,
    };
    let (conversation, merged) = conversations::link(conn, id, m.account_id, &threading)?;
    // A muted conversation stays muted when mail joins it to another.
    for from in merged {
        if conn.execute("DELETE FROM muted WHERE conversation = ?1", [from])? > 0 {
            conn.execute(
                "INSERT OR IGNORE INTO muted (conversation) VALUES (?1)",
                [conversation],
            )?;
        }
    }
    priority::arrived(conn, id)?;
    Ok(id)
}

/// Drops a folder (already gone from the server). Filed mail it held and
/// nothing else did goes with it, as the server deleted that mail too; but
/// not on Gmail, where a label's mail stays in All Mail. Messages it was the
/// location of lose that location; Done and Deleted mail stays.
fn forget_folder_in(conn: &Connection, folder_id: i64) -> Result<usize> {
    conn.execute(
        "DELETE FROM messages
         WHERE id IN (SELECT message_id FROM folder_members WHERE folder_id = ?1)
           AND remote_place = 'folder' AND changed_at = 0
           AND (SELECT kind FROM accounts WHERE id = messages.account_id) != 'gmail'
           AND (mailbox IS NULL OR mailbox = (SELECT remote_id FROM folders WHERE id = ?1))
           AND NOT EXISTS (SELECT 1 FROM folder_members o
                           WHERE o.message_id = messages.id AND o.folder_id != ?1)",
        [folder_id],
    )?;
    conn.execute(
        "UPDATE messages SET mailbox = NULL, uid = NULL
         WHERE (account_id, mailbox) = (SELECT account_id, remote_id FROM folders WHERE id = ?1)",
        [folder_id],
    )?;
    conn.execute("DELETE FROM folders WHERE id = ?1", [folder_id])
}

/// A folder and everything nested under it, as (id, remote id, parent remote id).
fn folder_subtree(conn: &Connection, folder_id: i64) -> Result<Vec<(i64, String, Option<String>)>> {
    let account_id: i64 = conn.query_row(
        "SELECT account_id FROM folders WHERE id = ?1",
        [folder_id],
        |r| r.get(0),
    )?;
    let mut stmt =
        conn.prepare("SELECT id, remote_id, parent_remote FROM folders WHERE account_id = ?1")?;
    let all: Vec<(i64, String, Option<String>)> = stmt
        .query_map([account_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_>>()?;
    let mut subtree: Vec<(i64, String, Option<String>)> =
        all.iter().filter(|f| f.0 == folder_id).cloned().collect();
    let mut i = 0;
    while i < subtree.len() {
        let parent = subtree[i].1.clone();
        subtree.extend(
            all.iter()
                .filter(|f| f.2.as_deref() == Some(parent.as_str()))
                .cloned(),
        );
        i += 1;
    }
    Ok(subtree)
}

fn folder_row(r: &Row) -> Result<Folder> {
    Ok(Folder {
        id: r.get(0)?,
        account_id: r.get(1)?,
        remote_id: r.get(2)?,
        parent_id: r.get(3)?,
        name: r.get(4)?,
        role: r
            .get::<_, Option<String>>(5)?
            .as_deref()
            .and_then(FolderRole::parse),
        selectable: r.get(6)?,
        sync_state: r.get(7)?,
        synced_at: r.get(8)?,
    })
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
        reply_to: from_json(&r.get::<_, String>(19)?),
        subject: r.get(11)?,
        snippet: r.get(12)?,
        date: r.get(13)?,
        attachment: attachments.into_iter().next().map(|a| a.name),
        folders: r
            .get::<_, Option<String>>(15)?
            .map(|ids| ids.split(',').filter_map(|id| id.parse().ok()).collect())
            .unwrap_or_default(),
        folder: r.get(16)?,
        to: from_json(&r.get::<_, String>(17)?),
        conversation: r.get(18)?,
        returned_at: r.get(20)?,
        muted: r.get(21)?,
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
        MessageState::Filed => "filed",
    }
}

fn parse_state(s: &str) -> MessageState {
    match s {
        "done" => MessageState::Done,
        "snoozed" => MessageState::Snoozed,
        "aside" => MessageState::Aside,
        "deleted" => MessageState::Deleted,
        "blocked" => MessageState::Blocked,
        "filed" => MessageState::Filed,
        _ => MessageState::Inbox,
    }
}

fn place_str(p: Place) -> &'static str {
    match p {
        Place::Inbox => "inbox",
        Place::Archive => "archive",
        Place::Trash => "trash",
        Place::Blocked => "blocked",
        Place::Folder => "folder",
    }
}

fn parse_place(s: &str) -> Place {
    match s {
        "archive" => Place::Archive,
        "trash" => Place::Trash,
        "blocked" => Place::Blocked,
        "folder" => Place::Folder,
        _ => Place::Inbox,
    }
}

#[cfg(test)]
mod tests;
