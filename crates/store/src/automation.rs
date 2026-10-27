//! Inbox automation, Spark's Auto-Labels and Auto-Drafts. AI sorts new
//! inbox mail under one of Spark's eight labels and says whether it needs a
//! reply; for those that do, a reply waits in Drafts. Spark does this on its
//! servers; here the app does it with the user's own AI provider, and this
//! keeps what it decided.
//!
//! Every email is looked at once (`automation_checked`). The label it got
//! is kept here (`auto_labels`), so it shows in every account; Gmail and
//! Microsoft accounts also get it on the server, as a Gmail label or an
//! Outlook category, through `label_pushes`. An auto-draft is a saved draft
//! like any other, with the options it was written with (`auto_drafts`).

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{Result, Store, json, now_ms, refused};

/// Account `?1`'s mail to look at (`Store::automation_candidates`). The `+`
/// keeps SQLite off the (account, conversation) index, which would visit
/// all the account's mail every minute; the date index reaches only what's
/// recent.
pub(crate) const CANDIDATES: &str = "SELECT m.id, m.account_id, m.date FROM messages m
             WHERE +m.account_id = ?1 AND m.state = 'inbox' AND m.folder_only = 0 AND m.date >= ?2
               AND NOT EXISTS (SELECT 1 FROM accounts a WHERE a.email = m.from_address COLLATE NOCASE)
               AND NOT EXISTS (SELECT 1 FROM automation_checked c WHERE c.message_id = m.id
                               AND (c.checked_at IS NOT NULL OR c.retry_at > ?3))
               AND NOT EXISTS (SELECT 1 FROM draft_uploads d WHERE d.message_id = m.id)
               AND NOT EXISTS (SELECT 1 FROM folder_members fm JOIN folders f ON f.id = fm.folder_id
                               WHERE fm.message_id = m.id AND f.role = 'drafts')
               AND m.id NOT IN (SELECT draft_message FROM outbox WHERE draft_message IS NOT NULL)
             ORDER BY m.date DESC LIMIT ?4";

/// Account `?1`'s queued labels. The queue is short and the account's mail
/// isn't: CROSS JOIN keeps SQLite reading the queue, not the mailbox.
pub(crate) const LABEL_PUSHES: &str = "SELECT p.message_id, p.name, p.adding FROM label_pushes p
     CROSS JOIN messages m ON m.id = p.message_id
     WHERE m.account_id = ?1 ORDER BY p.rowid";

/// Whether account `?1` has queued labels, read the same way.
pub(crate) const LABEL_WORK: &str = "SELECT EXISTS (SELECT 1 FROM label_pushes p
     CROSS JOIN messages m ON m.id = p.message_id WHERE m.account_id = ?1)";

/// What `auto_drafts.written` says of a draft the user changed, here or in
/// another app: no saved draft matches it (`saved_text` is JSON), so it's
/// never cleaned up, and its other options can't be picked over it.
const EDITED: &str = "\n";

/// What `auto_labels` keeps for an email whose label the user took off:
/// none shows, and inbox automation doesn't give it one again.
const TAKEN_OFF: &str = "";

/// For a query about email `?1`: `o`, the email (its account, its
/// conversation `c`, its date), and `members`, the ids of its conversation's
/// emails. Looked up by account and conversation, as indexed; the email a
/// conversation started with, whose id it is, may have none set.
const MEMBERS: &str = "o AS (SELECT account_id, COALESCE(conversation, id) AS c, date
                             FROM messages WHERE id = ?1),
     members AS (SELECT m.id FROM o JOIN messages m
                 ON m.account_id = o.account_id AND m.conversation = o.c
                 UNION SELECT c FROM o)";

/// Email `m` isn't a draft (saved here, or in a Drafts folder).
const NOT_DRAFT: &str = "NOT EXISTS (SELECT 1 FROM draft_uploads u WHERE u.message_id = m.id)
     AND NOT EXISTS (SELECT 1 FROM folder_members fm JOIN folders f ON f.id = fm.folder_id
                     WHERE fm.message_id = m.id AND f.role = 'drafts')";

/// Version 23: what inbox automation decided, and its lists. Safe to run
/// again over itself.
pub(crate) const SCHEMA_AUTOMATION: &str = "
-- Mail inbox automation has looked at, and what AI said of it. A failed
-- look leaves checked_at NULL until retry_at, a few times at most.
CREATE TABLE IF NOT EXISTS automation_checked (
    message_id INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    checked_at INTEGER,
    label TEXT,
    needs_reply INTEGER NOT NULL DEFAULT 0,
    failures INTEGER NOT NULL DEFAULT 0,
    retry_at INTEGER
);

-- The Auto-Label a message shows: one at most, as in Spark.
CREATE TABLE IF NOT EXISTS auto_labels (
    message_id INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    label TEXT NOT NULL
);

-- Labels to put on or take off the server copy, by the server's name.
CREATE TABLE IF NOT EXISTS label_pushes (
    message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    adding INTEGER NOT NULL,
    PRIMARY KEY (message_id, name)
);

-- Drafts inbox automation wrote, and the email each replies to. `written`
-- is the text as written, which tells an untouched draft from an edited one.
CREATE TABLE IF NOT EXISTS auto_drafts (
    draft_id INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    options TEXT NOT NULL,
    chosen INTEGER NOT NULL DEFAULT 0,
    written TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS auto_drafts_message ON auto_drafts(message_id);

-- Senders Auto-Drafts leaves alone, in every account.
CREATE TABLE IF NOT EXISTS auto_draft_excluded (
    address TEXT PRIMARY KEY COLLATE NOCASE
);

-- The user's instructions for how drafts are written; with an account,
-- only for that account's mail.
CREATE TABLE IF NOT EXISTS auto_draft_instructions (
    id INTEGER PRIMARY KEY,
    text TEXT NOT NULL,
    account_id INTEGER REFERENCES accounts(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL
);
";

/// Version 24: the replies still to be written. `draft_due` is when to
/// write one (NULL: none to write), `draft_failures` how often it failed.
/// Adds only what's missing, so it's safe to run again over itself.
pub(crate) fn add_draft_queue(conn: &Connection) -> Result<()> {
    let has: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM pragma_table_info('automation_checked')
                        WHERE name = 'draft_due')",
        [],
        |r| r.get(0),
    )?;
    if !has {
        conn.execute_batch(
            "ALTER TABLE automation_checked ADD COLUMN draft_due INTEGER;
             ALTER TABLE automation_checked ADD COLUMN draft_failures INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS automation_draft_due ON automation_checked(draft_due)
             WHERE draft_due IS NOT NULL;",
    )
}

/// Version 25: the version of an auto-draft's server copy as this app put
/// it there (Graph's changeKey; for IMAP, where it went), to tell whether
/// another app changed it since. Adds only what's missing, so it's safe to run again over itself.
pub(crate) fn add_server_version(conn: &Connection) -> Result<()> {
    let has: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM pragma_table_info('auto_drafts')
                        WHERE name = 'server_version')",
        [],
        |r| r.get(0),
    )?;
    if !has {
        conn.execute_batch("ALTER TABLE auto_drafts ADD COLUMN server_version TEXT;")?;
    }
    Ok(())
}

/// Spark's Auto-Labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AutoLabel {
    ToRespond,
    Fyi,
    Comment,
    Notification,
    MeetingUpdate,
    Actioned,
    Marketing,
    News,
}

impl AutoLabel {
    pub const ALL: [AutoLabel; 8] = [
        AutoLabel::ToRespond,
        AutoLabel::Fyi,
        AutoLabel::Comment,
        AutoLabel::Notification,
        AutoLabel::MeetingUpdate,
        AutoLabel::Actioned,
        AutoLabel::Marketing,
        AutoLabel::News,
    ];

    /// The id AI answers with and the store keeps.
    pub fn as_str(self) -> &'static str {
        match self {
            AutoLabel::ToRespond => "toRespond",
            AutoLabel::Fyi => "fyi",
            AutoLabel::Comment => "comment",
            AutoLabel::Notification => "notification",
            AutoLabel::MeetingUpdate => "meetingUpdate",
            AutoLabel::Actioned => "actioned",
            AutoLabel::Marketing => "marketing",
            AutoLabel::News => "news",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.as_str() == s)
    }

    /// Its name, as the UI shows it and the server keeps it (a Gmail label,
    /// an Outlook category).
    pub fn name(self) -> &'static str {
        match self {
            AutoLabel::ToRespond => "To respond",
            AutoLabel::Fyi => "FYI",
            AutoLabel::Comment => "Comment",
            AutoLabel::Notification => "Notification",
            AutoLabel::MeetingUpdate => "Meeting update",
            AutoLabel::Actioned => "Actioned",
            AutoLabel::Marketing => "Marketing",
            AutoLabel::News => "News",
        }
    }
}

/// How far back Auto-Labels reaches when it's turned on for an account, as
/// Spark labels the past few days' mail too.
pub const LABEL_BACKFILL_MS: i64 = 7 * 86_400_000;

/// Mail that arrived just before Auto-Drafts was turned on still gets one.
pub const DRAFT_GRACE_MS: i64 = 3_600_000;

/// How many times a look that failed is tried again, and how long the
/// first wait is (each wait doubles).
const MAX_FAILURES: i64 = 3;
const RETRY_MS: i64 = 5 * 60_000;

/// Settings → Inbox Automation, kept in the `settings` table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AutomationSettings {
    /// Accounts with Auto-Labels on, with when it was turned on (Unix ms).
    pub label_accounts: BTreeMap<i64, i64>,
    /// Labels turned off: AI may still pick them, but they aren't applied.
    pub labels_off: Vec<AutoLabel>,
    /// Accounts with Auto-Drafts on, with when it was turned on.
    pub draft_accounts: BTreeMap<i64, i64>,
    /// Untouched auto-drafts are deleted after this many days; `None` keeps them.
    pub cleanup_days: Option<u32>,
    /// Drafts for senders still waiting in the Gatekeeper.
    pub gatekeeper_drafts: bool,
    /// The model to use instead of the one in Settings → AI.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl Default for AutomationSettings {
    fn default() -> Self {
        Self {
            label_accounts: BTreeMap::new(),
            labels_off: Vec::new(),
            draft_accounts: BTreeMap::new(),
            cleanup_days: Some(7),
            gatekeeper_drafts: false,
            model: None,
        }
    }
}

impl AutomationSettings {
    pub fn is_on(&self) -> bool {
        !self.label_accounts.is_empty() || !self.draft_accounts.is_empty()
    }

    pub fn labels(&self, account_id: i64) -> bool {
        self.label_accounts.contains_key(&account_id)
    }

    pub fn drafts(&self, account_id: i64) -> bool {
        self.draft_accounts.contains_key(&account_id)
    }

    /// Whether `label` is applied in `account_id`.
    pub fn applies(&self, account_id: i64, label: AutoLabel) -> bool {
        self.labels(account_id) && !self.labels_off.contains(&label)
    }

    /// The oldest mail looked at in each account: a week before Auto-Labels
    /// was turned on, or just before Auto-Drafts was.
    fn since(&self) -> BTreeMap<i64, i64> {
        let mut since = BTreeMap::new();
        for (&account, &on) in &self.label_accounts {
            since.insert(account, on - LABEL_BACKFILL_MS);
        }
        for (&account, &on) in &self.draft_accounts {
            let from = on - DRAFT_GRACE_MS;
            since
                .entry(account)
                .and_modify(|s: &mut i64| *s = (*s).min(from))
                .or_insert(from);
        }
        since
    }
}

/// An email inbox automation hasn't looked at yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub id: i64,
    pub account_id: i64,
}

/// What AI said of an email.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub label: Option<AutoLabel>,
    pub needs_reply: bool,
}

/// A label to put on or take off a message's server copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabelPush {
    pub message_id: i64,
    /// The server's name for it (`AutoLabel::name`).
    pub name: String,
    pub adding: bool,
}

/// One of the replies an auto-draft offers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftOption {
    /// A few words for the picker; empty when there's only one.
    #[serde(default)]
    pub title: String,
    pub body: String,
}

/// A draft inbox automation wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutoDraft {
    pub draft_id: i64,
    /// The email it replies to.
    pub message_id: i64,
    pub options: Vec<DraftOption>,
    /// The option the draft says now.
    pub chosen: usize,
    pub created_at: i64,
}

/// One of the user's instructions for Auto-Drafts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftInstruction {
    pub id: i64,
    pub text: String,
    /// Only for this account's mail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<i64>,
    pub created_at: i64,
}

impl Store {
    pub fn automation_settings(&self) -> Result<AutomationSettings> {
        settings(&self.conn)
    }

    /// Saves the settings. An account whose Auto-Labels just came on gets
    /// the labels AI already gave its past week's mail.
    pub fn set_automation_settings(&mut self, settings: &AutomationSettings) -> Result<()> {
        let tx = self.conn.transaction()?;
        let before = self::settings(&tx)?;
        tx.execute(
            "INSERT INTO settings (key, value) VALUES ('inboxAutomation', ?1)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [json(settings)],
        )?;
        for (&account, &on) in &settings.label_accounts {
            if !before.labels(account) {
                apply_checked(&tx, settings, account, on - LABEL_BACKFILL_MS)?;
            }
        }
        // Mail AI said needs a reply, from just before Auto-Drafts came on:
        // due now, as if it had been on when AI looked.
        for (&account, &on) in &settings.draft_accounts {
            if !before.drafts(account) {
                tx.execute(
                    "UPDATE automation_checked SET draft_due = ?2, draft_failures = 0
                     WHERE needs_reply = 1 AND draft_due IS NULL
                       AND message_id IN (SELECT id FROM messages
                                          WHERE account_id = ?1 AND date >= ?3)",
                    params![account, on, on - DRAFT_GRACE_MS],
                )?;
            }
        }
        tx.commit()
    }

    /// Up to `limit` emails to look at, newest first: inbox mail from
    /// someone else, in accounts with inbox automation on, since it was
    /// turned on there (`AutomationSettings::since`). Not drafts, and not
    /// those already looked at or waiting to be tried again after `now`.
    pub fn automation_candidates(
        &self,
        settings: &AutomationSettings,
        now: i64,
        limit: usize,
    ) -> Result<Vec<Candidate>> {
        let mut found: Vec<(Candidate, i64)> = Vec::new();
        let mut stmt = self.conn.prepare_cached(CANDIDATES)?;
        for (account, since) in settings.since() {
            let rows = stmt.query_map(params![account, since, now, limit as i64], |r| {
                let candidate = Candidate {
                    id: r.get(0)?,
                    account_id: r.get(1)?,
                };
                Ok((candidate, r.get(2)?))
            })?;
            for row in rows {
                found.push(row?);
            }
        }
        // Each account's newest, then the newest of them all.
        found.sort_by_key(|(c, date)| (std::cmp::Reverse(*date), c.id));
        found.truncate(limit);
        Ok(found.into_iter().map(|(c, _)| c).collect())
    }

    /// Records what AI said of an email, and applies its label if the
    /// settings say so; one looked at already stays as it is. Returns
    /// whether the label shown changed.
    pub fn record_verdict(
        &mut self,
        id: i64,
        verdict: Verdict,
        settings: &AutomationSettings,
        push: bool,
        now: i64,
    ) -> Result<bool> {
        let tx = self.conn.transaction()?;
        // Looked at already: answered while AI was at it (`user_replied`).
        let checked: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM automation_checked
                            WHERE message_id = ?1 AND checked_at IS NOT NULL)",
            [id],
            |r| r.get(0),
        )?;
        if checked {
            return Ok(false);
        }
        let account: Option<i64> = tx
            .query_row("SELECT account_id FROM messages WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        // A reply is due for one that needs it, in an account with Auto-Drafts on.
        let draft_due =
            (verdict.needs_reply && account.is_some_and(|a| settings.drafts(a))).then_some(now);
        tx.execute(
            "INSERT INTO automation_checked (message_id, checked_at, label, needs_reply, draft_due)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (message_id) DO UPDATE SET checked_at = excluded.checked_at,
                 label = excluded.label, needs_reply = excluded.needs_reply, retry_at = NULL,
                 draft_due = excluded.draft_due, draft_failures = 0",
            params![
                id,
                now,
                verdict.label.map(AutoLabel::as_str),
                verdict.needs_reply,
                draft_due
            ],
        )?;
        let mut changed = false;
        if let (Some(account), Some(label)) = (account, verdict.label)
            // A label the user chose, or took off, stays as they left it.
            && !has_label_row(&tx, id)?
            && let Some(label) = answered_as(&tx, settings, account, id, label)?
        {
            changed = set_label(&tx, id, Some(label), push)?;
        }
        tx.commit()?;
        Ok(changed)
    }

    /// Up to `limit` emails a reply is due for (`record_verdict`), newest
    /// first, in accounts with Auto-Drafts on and since it was turned on.
    /// Mail from senders waiting in the Gatekeeper waits too, unless the
    /// settings draft for them: once the sender is accepted, it's due.
    /// Whether one is still wanted is for the caller to check again.
    pub fn draft_candidates(
        &self,
        settings: &AutomationSettings,
        now: i64,
        limit: usize,
    ) -> Result<Vec<i64>> {
        let mut found: Vec<(i64, i64)> = Vec::new();
        let mut stmt = self.conn.prepare_cached(
            "SELECT c.message_id, m.date FROM automation_checked c
             CROSS JOIN messages m ON m.id = c.message_id
             WHERE c.draft_due IS NOT NULL AND c.draft_due <= ?3
               AND m.account_id = ?1 AND m.date >= ?2 AND (m.screening = 0 OR ?5)
             ORDER BY m.date DESC LIMIT ?4",
        )?;
        for (&account, &on) in &settings.draft_accounts {
            let rows = stmt.query_map(
                params![
                    account,
                    on - DRAFT_GRACE_MS,
                    now,
                    limit as i64,
                    settings.gatekeeper_drafts
                ],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            for row in rows {
                found.push(row?);
            }
        }
        found.sort_by_key(|&(id, date)| (std::cmp::Reverse(date), id));
        found.truncate(limit);
        Ok(found.into_iter().map(|(id, _)| id).collect())
    }

    /// No reply is due for email `id` any more: one was written, or isn't
    /// wanted after all.
    pub fn draft_settled(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE automation_checked SET draft_due = NULL WHERE message_id = ?1",
            [id],
        )?;
        Ok(())
    }

    /// Writing a reply to email `id` failed: it's tried again later, a few
    /// times, as a look is.
    pub fn draft_failed(&self, id: i64, now: i64) -> Result<()> {
        let failures: i64 = self
            .conn
            .query_row(
                "SELECT draft_failures FROM automation_checked WHERE message_id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0)
            + 1;
        let due = (failures < MAX_FAILURES).then(|| now + RETRY_MS * (1 << (failures - 1)));
        // Not if it stopped being due meanwhile (the user replied).
        self.conn.execute(
            "UPDATE automation_checked SET draft_due = ?2, draft_failures = ?3
             WHERE message_id = ?1 AND draft_due IS NOT NULL",
            params![id, due, failures],
        )?;
        Ok(())
    }

    /// Draft `draft_id` was put on the server as `version` (Graph's
    /// changeKey; for IMAP, its mailbox and UID): if it's an auto-draft,
    /// that's the copy that's its own.
    pub fn auto_draft_uploaded(&self, draft_id: i64, version: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE auto_drafts SET server_version = ?2 WHERE draft_id = ?1",
            params![draft_id, version],
        )?;
        Ok(())
    }

    /// The version of auto-draft `draft_id`'s server copy as this app last
    /// put it there, if it did.
    pub fn auto_draft_server_version(&self, draft_id: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT server_version FROM auto_drafts WHERE draft_id = ?1",
                [draft_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }

    /// The user changed draft `draft_id`, in the composer or another app:
    /// if it's an auto-draft, it's theirs now, kept from cleanup and from
    /// picking another option over it.
    pub fn auto_draft_edited(&self, draft_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE auto_drafts SET written = ?2 WHERE draft_id = ?1",
            params![draft_id, EDITED],
        )?;
        Ok(())
    }

    /// Whether auto-draft `draft_id` still says exactly what it was written
    /// with: then picking another option can't lose the user's edits.
    pub fn auto_draft_untouched(&self, draft_id: i64) -> Result<bool> {
        let written: Option<String> = self
            .conn
            .query_row(
                "SELECT written FROM auto_drafts WHERE draft_id = ?1",
                [draft_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(written) = written else {
            return Ok(false);
        };
        Ok(written == saved_text(&self.conn, draft_id)?)
    }

    /// A look at an email that failed: it's tried again later, a few times.
    pub fn record_check_failed(&self, id: i64, now: i64) -> Result<()> {
        let failures: i64 = self
            .conn
            .query_row(
                "SELECT failures FROM automation_checked WHERE message_id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0)
            + 1;
        let (checked_at, retry_at) = if failures >= MAX_FAILURES {
            (Some(now), None)
        } else {
            (None, Some(now + RETRY_MS * (1 << (failures - 1))))
        };
        // One looked at meanwhile (answered by the user) stays so.
        self.conn.execute(
            "INSERT INTO automation_checked (message_id, checked_at, failures, retry_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (message_id) DO UPDATE SET checked_at = excluded.checked_at,
                 failures = excluded.failures, retry_at = excluded.retry_at
             WHERE automation_checked.checked_at IS NULL",
            params![id, checked_at, failures, retry_at],
        )?;
        Ok(())
    }

    /// What AI said of an email, once it has looked.
    pub fn verdict(&self, id: i64) -> Result<Option<Verdict>> {
        self.conn
            .query_row(
                "SELECT label, needs_reply FROM automation_checked
                 WHERE message_id = ?1 AND checked_at IS NOT NULL",
                [id],
                |r| {
                    Ok(Verdict {
                        label: r
                            .get::<_, Option<String>>(0)?
                            .as_deref()
                            .and_then(AutoLabel::parse),
                        needs_reply: r.get(1)?,
                    })
                },
            )
            .optional()
    }

    pub fn auto_label(&self, id: i64) -> Result<Option<AutoLabel>> {
        label_of(&self.conn, id)
    }

    /// Shows `label` on an email instead of the one it has (`None` takes it
    /// off), as the user chose: inbox automation leaves it that way. With
    /// `push` the server's copy follows. Returns whether anything changed.
    pub fn set_auto_label(
        &mut self,
        id: i64,
        label: Option<AutoLabel>,
        push: bool,
    ) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM messages WHERE id = ?1)",
            [id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(refused("That email is gone."));
        }
        let changed = set_label(&tx, id, label, push)?;
        if label.is_none() {
            tx.execute(
                "INSERT INTO auto_labels (message_id, label) VALUES (?1, ?2)
                 ON CONFLICT (message_id) DO UPDATE SET label = excluded.label",
                params![id, TAKEN_OFF],
            )?;
        }
        tx.commit()?;
        Ok(changed)
    }

    /// The user sent a reply to email `id` at `at`: its conversation is
    /// answered before a sync brings the reply here for `answered_after` to
    /// find. To respond becomes Actioned (no label, if that's off); its mail
    /// from before then that AI hasn't looked at yet won't be, and is
    /// Actioned too; and none of it needs a reply any more, nor gets one
    /// when Auto-Drafts comes on, nor To respond when Auto-Labels does.
    /// Returns the emails whose label changed.
    pub fn user_replied(
        &mut self,
        id: i64,
        settings: &AutomationSettings,
        push: bool,
        at: i64,
    ) -> Result<Vec<i64>> {
        let tx = self.conn.transaction()?;
        let account: Option<i64> = tx
            .query_row("SELECT account_id FROM messages WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        // Inbox automation is off there.
        let Some((account, since)) =
            account.and_then(|a| settings.since().get(&a).map(|&since| (a, since)))
        else {
            return Ok(Vec::new());
        };
        let actioned = settings
            .applies(account, AutoLabel::Actioned)
            .then_some(AutoLabel::Actioned);
        let mut changed = Vec::new();
        if settings.labels(account) {
            let ids: Vec<i64> = tx
                .prepare(&format!(
                    "WITH {MEMBERS}
                     SELECT l.message_id FROM members
                     JOIN auto_labels l ON l.message_id = members.id WHERE l.label = ?2"
                ))?
                .query_map(params![id, AutoLabel::ToRespond.as_str()], |r| r.get(0))?
                .collect::<Result<_>>()?;
            for id in ids {
                if set_label(&tx, id, actioned, push)? {
                    changed.push(id);
                }
            }
        }
        // What AI would look at next (`CANDIDATES`), from before the reply.
        let unchecked: Vec<i64> = tx
            .prepare(&format!(
                "WITH {MEMBERS}
                 SELECT m.id FROM members JOIN messages m ON m.id = members.id
                 WHERE m.state = 'inbox' AND m.folder_only = 0 AND m.date >= ?2 AND m.date <= ?3
                   AND NOT EXISTS (SELECT 1 FROM accounts a
                                   WHERE a.email = m.from_address COLLATE NOCASE)
                   AND NOT EXISTS (SELECT 1 FROM automation_checked c
                                   WHERE c.message_id = m.id AND c.checked_at IS NOT NULL)
                   AND {NOT_DRAFT}"
            ))?
            .query_map(params![id, since, at], |r| r.get(0))?
            .collect::<Result<_>>()?;
        for id in unchecked {
            tx.execute(
                "INSERT INTO automation_checked (message_id, checked_at) VALUES (?1, ?2)
                 ON CONFLICT (message_id) DO UPDATE SET checked_at = excluded.checked_at,
                     retry_at = NULL",
                params![id, at],
            )?;
            if actioned.is_some() && !has_label_row(&tx, id)? && set_label(&tx, id, actioned, push)?
            {
                changed.push(id);
            }
        }
        tx.execute(
            &format!(
                "WITH {MEMBERS}
                 UPDATE automation_checked SET draft_due = NULL, needs_reply = 0,
                     label = CASE WHEN label = ?3 THEN ?4 ELSE label END
                 WHERE (draft_due IS NOT NULL OR needs_reply = 1 OR label = ?3) AND message_id IN
                     (SELECT m.id FROM members JOIN messages m ON m.id = members.id
                      WHERE m.date <= ?2)"
            ),
            params![
                id,
                at,
                AutoLabel::ToRespond.as_str(),
                AutoLabel::Actioned.as_str()
            ],
        )?;
        tx.commit()?;
        Ok(changed)
    }

    /// Whether a reply is still due for email `id` (`record_verdict`).
    pub fn reply_due(&self, id: i64) -> Result<bool> {
        self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM automation_checked
                            WHERE message_id = ?1 AND draft_due IS NOT NULL)",
            [id],
            |r| r.get(0),
        )
    }

    /// Labels waiting to go to account `account_id`'s server.
    pub fn label_pushes(&self, account_id: i64) -> Result<Vec<LabelPush>> {
        let mut stmt = self.conn.prepare_cached(LABEL_PUSHES)?;
        stmt.query_map([account_id], |r| {
            Ok(LabelPush {
                message_id: r.get(0)?,
                name: r.get(1)?,
                adding: r.get(2)?,
            })
        })?
        .collect()
    }

    /// Asked on every sync round, mostly with nothing queued.
    pub fn has_label_work(&self, account_id: i64) -> Result<bool> {
        self.conn
            .prepare_cached(LABEL_WORK)?
            .query_row([account_id], |r| r.get(0))
    }

    /// The server has `push` now, or can't take it: it's off the queue,
    /// unless it changed meanwhile.
    pub fn label_pushed(&self, push: &LabelPush) -> Result<()> {
        self.conn.execute(
            "DELETE FROM label_pushes WHERE message_id = ?1 AND name = ?2 AND adding = ?3",
            params![push.message_id, push.name, push.adding],
        )?;
        Ok(())
    }

    /// Keeps a draft inbox automation wrote, replying to `message_id`, once
    /// it's saved; `chosen` is the option it says.
    pub fn add_auto_draft(
        &self,
        draft_id: i64,
        message_id: i64,
        options: &[DraftOption],
        chosen: usize,
        now: i64,
    ) -> Result<()> {
        let written = saved_text(&self.conn, draft_id)?;
        self.conn.execute(
            "INSERT INTO auto_drafts (draft_id, message_id, options, chosen, written, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (draft_id) DO UPDATE SET message_id = excluded.message_id,
                 options = excluded.options, chosen = excluded.chosen,
                 written = excluded.written, created_at = excluded.created_at",
            params![
                draft_id,
                message_id,
                json(options),
                chosen as i64,
                written,
                now
            ],
        )?;
        Ok(())
    }

    /// The auto-draft with this draft id.
    pub fn auto_draft(&self, draft_id: i64) -> Result<Option<AutoDraft>> {
        self.conn
            .query_row(
                "SELECT draft_id, message_id, options, chosen, created_at FROM auto_drafts
                 WHERE draft_id = ?1",
                [draft_id],
                auto_draft_row,
            )
            .optional()
    }

    /// The newest auto-draft replying to `message_id`.
    pub fn auto_draft_for(&self, message_id: i64) -> Result<Option<AutoDraft>> {
        self.conn
            .query_row(
                "SELECT draft_id, message_id, options, chosen, created_at FROM auto_drafts
                 WHERE message_id = ?1 ORDER BY created_at DESC, draft_id DESC LIMIT 1",
                [message_id],
                auto_draft_row,
            )
            .optional()
    }

    /// The draft was saved saying option `chosen`: what it says now is what
    /// it was written as.
    pub fn auto_draft_chosen(&self, draft_id: i64, chosen: usize) -> Result<bool> {
        let Some(draft) = self.auto_draft(draft_id)? else {
            return Ok(false);
        };
        if chosen >= draft.options.len() {
            return Err(refused("That draft has no such option."));
        }
        self.conn.execute(
            "UPDATE auto_drafts SET chosen = ?2, written = ?3 WHERE draft_id = ?1",
            params![draft_id, chosen as i64, saved_text(&self.conn, draft_id)?],
        )?;
        Ok(true)
    }

    /// Auto-drafts written before `before` that still say exactly what they
    /// were written with, for the cleanup: drafts the user edited stay.
    pub fn untouched_auto_drafts(&self, before: i64) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT a.draft_id, a.written FROM auto_drafts a
             JOIN messages m ON m.id = a.draft_id
             WHERE a.created_at < ?1
               AND a.draft_id NOT IN (SELECT draft_message FROM outbox WHERE draft_message IS NOT NULL)",
        )?;
        let rows: Vec<(i64, String)> = stmt
            .query_map([before], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_>>()?;
        let mut untouched = Vec::new();
        for (id, written) in rows {
            if saved_text(&self.conn, id)? == written {
                untouched.push(id);
            }
        }
        Ok(untouched)
    }

    /// Whether a draft already answers the conversation of email `id`: an
    /// auto-draft, or one of the user's replying to any of its emails. An
    /// auto-draft out of Drafts here was deleted in another app if the
    /// account's Drafts was listed whole (`complete` in its sync state);
    /// otherwise it may only be past the newest a listing holds, and counts.
    pub fn conversation_has_draft(&self, id: i64) -> Result<bool> {
        self.conn.query_row(
            &format!(
                "WITH {MEMBERS},
                      drafts AS (SELECT message_id AS id FROM draft_uploads
                                 UNION SELECT fm.message_id FROM folder_members fm
                                 JOIN folders f ON f.id = fm.folder_id WHERE f.role = 'drafts')
                 SELECT EXISTS (SELECT 1 FROM auto_drafts a JOIN members ON members.id = a.message_id, o
                                WHERE a.draft_id IN (SELECT id FROM drafts)
                                   OR NOT EXISTS (SELECT 1 FROM folders f
                                                  WHERE f.account_id = o.account_id AND f.role = 'drafts'
                                                    AND CASE WHEN json_valid(f.sync_state)
                                                        THEN json_extract(f.sync_state, '$.complete')
                                                        END IS 1))
                     OR EXISTS (SELECT 1 FROM drafts JOIN messages d ON d.id = drafts.id, o
                                WHERE d.account_id = o.account_id
                                  AND trim(d.in_reply_to, '<> ') IN
                                      (SELECT trim(m.message_id, '<> ') FROM messages m
                                       JOIN members ON members.id = m.id
                                       WHERE m.message_id IS NOT NULL))"
            ),
            [id],
            |r| r.get(0),
        )
    }

    /// Whether the user wrote in email `id`'s conversation after it: then
    /// there's nothing left to reply to.
    pub fn answered_after(&self, id: i64) -> Result<bool> {
        answered_after(&self.conn, id)
    }

    /// Whether someone else wrote in email `id`'s conversation after it (a
    /// request, then "never mind"): a reply is for the newest, as AI found
    /// it, not this one.
    pub fn written_after(&self, id: i64) -> Result<bool> {
        self.conn.query_row(
            &format!(
                "WITH {MEMBERS}
                 SELECT EXISTS (
                     SELECT 1 FROM members JOIN messages m ON m.id = members.id, o
                     WHERE m.id != ?1 AND m.date > o.date AND m.state NOT IN ('deleted', 'blocked')
                       AND NOT EXISTS (SELECT 1 FROM accounts a
                                       WHERE a.email = m.from_address COLLATE NOCASE)
                       AND {NOT_DRAFT})"
            ),
            [id],
            |r| r.get(0),
        )
    }

    /// Up to `limit` emails of email `id`'s conversation from before it,
    /// oldest first, drafts left out: what a reply to it reads first.
    pub fn conversation_before(&self, id: i64, limit: usize) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(&format!(
            "WITH {MEMBERS}
             SELECT m.id FROM members JOIN messages m ON m.id = members.id, o
             WHERE m.id != ?1 AND m.date <= o.date AND {NOT_DRAFT}
             ORDER BY m.date DESC, m.id DESC LIMIT ?2"
        ))?;
        let mut ids: Vec<i64> = stmt
            .query_map(params![id, limit as i64], |r| r.get(0))?
            .collect::<Result<_>>()?;
        ids.reverse();
        Ok(ids)
    }

    /// The senders Auto-Drafts leaves alone, in alphabetical order.
    pub fn auto_draft_excluded(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT address FROM auto_draft_excluded ORDER BY address")?;
        stmt.query_map([], |r| r.get(0))?.collect()
    }

    pub fn is_auto_draft_excluded(&self, address: &str) -> Result<bool> {
        self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM auto_draft_excluded WHERE address = ?1)",
            [address.trim()],
            |r| r.get(0),
        )
    }

    /// Leaves a sender's mail alone (`excluded`), or no longer does.
    pub fn set_auto_draft_excluded(&self, address: &str, excluded: bool) -> Result<()> {
        let address = address.trim();
        if !address.contains('@') {
            return Err(refused("That isn't an email address."));
        }
        if excluded {
            self.conn.execute(
                "INSERT OR IGNORE INTO auto_draft_excluded (address) VALUES (?1)",
                [address],
            )?;
        } else {
            self.conn.execute(
                "DELETE FROM auto_draft_excluded WHERE address = ?1",
                [address],
            )?;
        }
        Ok(())
    }

    /// The user's instructions for Auto-Drafts, oldest first.
    pub fn draft_instructions(&self) -> Result<Vec<DraftInstruction>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, text, account_id, created_at FROM auto_draft_instructions ORDER BY id",
        )?;
        stmt.query_map([], |r| {
            Ok(DraftInstruction {
                id: r.get(0)?,
                text: r.get(1)?,
                account_id: r.get(2)?,
                created_at: r.get(3)?,
            })
        })?
        .collect()
    }

    pub fn add_draft_instruction(
        &self,
        text: &str,
        account_id: Option<i64>,
    ) -> Result<DraftInstruction> {
        let text = text.trim();
        if text.is_empty() {
            return Err(refused("Write the instruction first."));
        }
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO auto_draft_instructions (text, account_id, created_at) VALUES (?1, ?2, ?3)",
            params![text, account_id, now],
        )?;
        Ok(DraftInstruction {
            id: self.conn.last_insert_rowid(),
            text: text.to_owned(),
            account_id,
            created_at: now,
        })
    }

    pub fn remove_draft_instruction(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM auto_draft_instructions WHERE id = ?1", [id])?;
        Ok(())
    }
}

/// Takes a removed account out of the settings.
pub(crate) fn forget_account(conn: &Connection, account_id: i64) -> Result<()> {
    let mut settings = settings(conn)?;
    let labels = settings.label_accounts.remove(&account_id).is_some();
    let drafts = settings.draft_accounts.remove(&account_id).is_some();
    if labels || drafts {
        conn.execute(
            "UPDATE settings SET value = ?1 WHERE key = 'inboxAutomation'",
            [json(&settings)],
        )?;
    }
    Ok(())
}

fn settings(conn: &Connection) -> Result<AutomationSettings> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'inboxAutomation'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(value
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default())
}

fn answered_after(conn: &Connection, id: i64) -> Result<bool> {
    conn.query_row(
        &format!(
            "WITH {MEMBERS}
             SELECT EXISTS (
                 SELECT 1 FROM members JOIN messages m ON m.id = members.id, o
                 JOIN accounts a ON a.email = m.from_address COLLATE NOCASE
                 WHERE m.id != ?1 AND m.date > o.date AND {NOT_DRAFT})"
        ),
        [id],
        |r| r.get(0),
    )
}

/// The label AI's `label` becomes on email `id`, if the settings apply it:
/// To respond on mail the user has answered since (before AI looked, or in
/// the past week's mail labeled as it's turned on) is Actioned, as replying
/// would have made it.
fn answered_as(
    conn: &Connection,
    settings: &AutomationSettings,
    account_id: i64,
    id: i64,
    label: AutoLabel,
) -> Result<Option<AutoLabel>> {
    let label = if label == AutoLabel::ToRespond && answered_after(conn, id)? {
        AutoLabel::Actioned
    } else {
        label
    };
    Ok(settings.applies(account_id, label).then_some(label))
}

/// Whether email `id` has a label the user or AI gave it, or one the user
/// took off.
fn has_label_row(conn: &Connection, id: i64) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM auto_labels WHERE message_id = ?1)",
        [id],
        |r| r.get(0),
    )
}

fn label_of(conn: &Connection, id: i64) -> Result<Option<AutoLabel>> {
    let label: Option<String> = conn
        .query_row(
            "SELECT label FROM auto_labels WHERE message_id = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(label.as_deref().and_then(AutoLabel::parse))
}

/// Shows `label` on email `id` (`None`: none), queueing the server's change
/// with `push`. Returns whether it changed.
fn set_label(conn: &Connection, id: i64, label: Option<AutoLabel>, push: bool) -> Result<bool> {
    let had = label_of(conn, id)?;
    if had == label {
        return Ok(false);
    }
    match label {
        Some(label) => conn.execute(
            "INSERT INTO auto_labels (message_id, label) VALUES (?1, ?2)
             ON CONFLICT (message_id) DO UPDATE SET label = excluded.label",
            params![id, label.as_str()],
        )?,
        None => conn.execute("DELETE FROM auto_labels WHERE message_id = ?1", [id])?,
    };
    if push {
        let mut queue = conn.prepare_cached(
            "INSERT INTO label_pushes (message_id, name, adding) VALUES (?1, ?2, ?3)
             ON CONFLICT (message_id, name) DO UPDATE SET adding = excluded.adding",
        )?;
        if let Some(had) = had {
            queue.execute(params![id, had.name(), false])?;
        }
        if let Some(label) = label {
            queue.execute(params![id, label.name(), true])?;
        }
    }
    Ok(true)
}

/// Applies the labels AI already gave account `account_id`'s mail since
/// `since`, where the user hasn't chosen one and the settings apply it.
fn apply_checked(
    conn: &Connection,
    settings: &AutomationSettings,
    account_id: i64,
    since: i64,
) -> Result<()> {
    let found: Vec<(i64, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT m.id, c.label, a.kind FROM automation_checked c
             JOIN messages m ON m.id = c.message_id JOIN accounts a ON a.id = m.account_id
             WHERE m.account_id = ?1 AND m.date >= ?2 AND c.label IS NOT NULL
               AND m.state NOT IN ('deleted', 'blocked')
               AND NOT EXISTS (SELECT 1 FROM auto_labels l WHERE l.message_id = m.id)",
        )?;
        stmt.query_map(params![account_id, since], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<Result<_>>()?
    };
    for (id, label, kind) in found {
        if let Some(label) = AutoLabel::parse(&label)
            && let Some(label) = answered_as(conn, settings, account_id, id, label)?
        {
            set_label(conn, id, Some(label), kind != "imap")?;
        }
    }
    Ok(())
}

fn auto_draft_row(r: &rusqlite::Row) -> Result<AutoDraft> {
    Ok(AutoDraft {
        draft_id: r.get(0)?,
        message_id: r.get(1)?,
        options: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default(),
        chosen: r.get::<_, i64>(3)?.max(0) as usize,
        created_at: r.get(4)?,
    })
}

/// Draft `id` as saved, to tell one the user hasn't touched from one they
/// have: its whole text (signature and all) without whitespace differences,
/// who it's to, its subject and its files.
fn saved_text(conn: &Connection, id: i64) -> Result<String> {
    type Saved = (String, String, String, String, String, String);
    let saved: Option<Saved> = conn
        .query_row(
            "SELECT text_body, to_list, cc_list, bcc_list, subject, attachments
             FROM messages WHERE id = ?1",
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
    Ok(saved
        .map(|(text, to, cc, bcc, subject, files)| {
            json(&(normalized(&text), to, cc, bcc, subject.trim(), files))
        })
        .unwrap_or_default())
}

/// Text without its whitespace differences, to compare a draft with what
/// was written: a save can change line endings and spacing.
fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests;
