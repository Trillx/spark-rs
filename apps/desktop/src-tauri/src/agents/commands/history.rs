//! What agents did: every command is written to `agent-history.jsonl` in
//! the app's data folder, with when it ran, who ran it, what it touched and
//! how it ended. Nothing leaves the computer, and entries go after 30 days.
//! `history` lists them; `history undo <id>` puts back what a triage
//! command changed, if its targets are still as it left them.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use spark_store::{
    Category, Change, Decision, Message, MessageState, Patch, PriorityUndo, RemovedReminder,
    Screen, Store, now_ms,
};

use super::{Args, Done, Env, Fail, Reply, db, parse_time, refuse, when};
use crate::agents::Access;
use crate::agents::tools::Effect;

pub const HISTORY_FILE: &str = "agent-history.jsonl";
const DAY_MS: i64 = 86_400_000;
const KEEP_MS: i64 = 30 * DAY_MS;
/// Longer strings in an entry's arguments are cut here (a draft's body).
const MAX_ARG: usize = 400;

/// The access a command needed, as Spark names the levels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Level {
    ReadOnly,
    Triage,
    Send,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Outcome {
    Success,
    Error,
    Blocked,
}

/// What a message was like, for undo.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    state: MessageState,
    snoozed_until: Option<i64>,
    unread: bool,
    pinned: bool,
    priority: bool,
    folder: Option<i64>,
}

impl Snapshot {
    pub fn of(m: &Message) -> Self {
        Self {
            state: m.state,
            snoozed_until: m.snoozed_until.filter(|_| m.state == MessageState::Snoozed),
            unread: m.unread,
            pinned: m.pinned,
            priority: m.priority,
            folder: m.folder,
        }
    }

    /// The patch that makes a message like this again.
    fn patch(&self) -> Patch {
        Patch {
            state: (self.state != MessageState::Filed || self.folder.is_none())
                .then_some(self.state),
            folder: self.folder.filter(|_| self.state == MessageState::Filed),
            snoozed_until: self.snoozed_until,
            unread: Some(self.unread),
            pinned: Some(self.pinned),
            priority: Some(self.priority),
        }
    }
}

/// One thing a command changed, and how to put it back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Step {
    Message {
        id: i64,
        before: Snapshot,
        after: Snapshot,
    },
    /// Undone only while the conversation is still as the agent left it.
    Muted {
        conversation: i64,
        before: bool,
    },
    Category {
        address: String,
        before: Option<Category>,
        after: Option<Category>,
    },
    Priority {
        undo: PriorityUndo,
    },
    /// `undo` puts back what `set` changed, for screens still as `set` left them.
    Gatekeeper {
        undo: Vec<Change>,
        #[serde(default)]
        set: Vec<Change>,
    },
    /// A draft the agent wrote, with `draft_print` of it then.
    Draft {
        id: i64,
        #[serde(default)]
        print: u64,
    },
    /// A reminder the agent set, for `at`. Its account and email
    /// (`Store::reminder_email`) tell it from a later reminder given its id.
    Reminder {
        id: i64,
        #[serde(default)]
        at: Option<i64>,
        #[serde(default)]
        account: i64,
        #[serde(default)]
        email: Option<(Option<String>, i64)>,
    },
    /// A reminder the agent cleared, all of it.
    ReminderCleared {
        reminder: RemovedReminder,
    },
    /// A reminder the agent moved from one time to another.
    ReminderMoved {
        id: i64,
        from: i64,
        to: i64,
        account: i64,
        email: Option<(Option<String>, i64)>,
    },
}

/// What a draft says, as a number that changes when it does: its subject,
/// To, Cc, Bcc, text, HTML and files (FNV-1a, which stays the same across
/// builds).
pub fn draft_print(store: &Store, id: i64) -> spark_store::Result<u64> {
    let subject = store.message(id)?.map(|m| m.subject).unwrap_or_default();
    let mut fields = vec![subject];
    if let Some(c) = store.content(id)? {
        fields.extend([c.to.join(","), c.cc.join(","), c.bcc.join(","), c.text]);
        fields.push(c.html.unwrap_or_default());
        // Its files by what they are; the server's ids for them come later.
        for a in &c.attachments {
            fields.push(format!("{}\u{1}{}\u{1}{}", a.name, a.mime, a.size));
        }
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in fields.join("\u{0}").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    Ok(hash)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub id: u64,
    pub at: i64,
    pub agent: String,
    pub command: String,
    pub args: Value,
    pub access: Level,
    pub result: Outcome,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
    /// Why it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub undo: Vec<Step>,
    #[serde(default)]
    pub undone: bool,
    /// The accounts agents could read when it ran: an agent sees the entry
    /// only while it can read all of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accounts: Vec<i64>,
}

impl Entry {
    fn visible(&self, readable: &[i64]) -> bool {
        self.accounts.iter().all(|a| readable.contains(a))
    }
}

/// A command as it ran, for its entry.
pub struct Run<'a> {
    pub agent: &'a str,
    pub command: &'a str,
    pub args: &'a Value,
    pub access: Level,
    pub at: i64,
    /// The accounts agents could read then (`Entry::accounts`).
    pub accounts: Vec<i64>,
}

/// The history file. Requests run at once, so writes take turns.
pub struct Log {
    path: PathBuf,
    /// The last entry's ID, once read.
    last: Mutex<Option<u64>>,
}

impl Log {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            last: Mutex::new(None),
        }
    }

    /// When the oldest entry was written, from the file's first line.
    fn oldest(&self) -> Option<i64> {
        use std::io::BufRead;
        let file = std::fs::File::open(&self.path).ok()?;
        let mut line = String::new();
        std::io::BufReader::new(file).read_line(&mut line).ok()?;
        serde_json::from_str::<Entry>(&line).ok().map(|e| e.at)
    }

    /// Entries newest last, leaving out lines that can't be read.
    fn read(&self) -> Vec<Entry> {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// Writes all of `entries` over the file.
    fn rewrite(&self, entries: &[Entry]) -> std::io::Result<()> {
        let temp = self.path.with_extension("jsonl.tmp");
        let mut text = String::new();
        for e in entries {
            text.push_str(&serde_json::to_string(e)?);
            text.push('\n');
        }
        std::fs::write(&temp, text)?;
        std::fs::rename(&temp, &self.path)
    }

    /// Adds a command's entry; the history failing to write mustn't fail
    /// the command, which has run.
    pub fn record(&self, run: Run, result: &Result<Reply, Fail>) {
        let Run {
            agent,
            command,
            args,
            access,
            at,
            accounts,
        } = run;
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        let id = match *last {
            Some(id) => id,
            None => self.read().iter().map(|e| e.id).max().unwrap_or(0),
        } + 1;
        let (outcome, detail, reply) = match result {
            Ok(reply) => (Outcome::Success, None, Some(reply)),
            Err(Fail::Blocked(m)) => (Outcome::Blocked, Some(m.clone()), None),
            Err(Fail::Error(m)) => (Outcome::Error, Some(m.clone()), None),
        };
        let entry = Entry {
            id,
            at,
            agent: agent.to_owned(),
            command: command.to_owned(),
            args: shorten(args),
            access,
            result: outcome,
            summary: reply.map(|r| r.summary.clone()).unwrap_or_default(),
            targets: reply.map(|r| r.targets.clone()).unwrap_or_default(),
            detail,
            undo: reply.map(|r| r.undo.clone()).unwrap_or_default(),
            undone: false,
            accounts,
        };
        // Pruned a day late, so it's rewritten at most once a day.
        let stale = self
            .oldest()
            .is_some_and(|oldest| oldest < at - KEEP_MS - DAY_MS);
        let written = if stale {
            let mut entries = self.read();
            entries.retain(|e| e.at >= at - KEEP_MS);
            entries.push(entry);
            self.rewrite(&entries)
        } else {
            serde_json::to_string(&entry)
                .map_err(std::io::Error::from)
                .and_then(|line| {
                    let mut file = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&self.path)?;
                    writeln!(file, "{line}")
                })
        };
        match written {
            Ok(()) => *last = Some(id),
            Err(e) => eprintln!("spark-rs: the agents' history couldn't be written: {e}"),
        }
    }
}

/// Arguments as recorded: long text cut short.
fn shorten(args: &Value) -> Value {
    match args {
        Value::String(s) if s.len() > MAX_ARG => {
            let mut at = MAX_ARG;
            while !s.is_char_boundary(at) {
                at -= 1;
            }
            Value::String(format!("{}…", &s[..at]))
        }
        Value::Array(items) => Value::Array(items.iter().map(shorten).collect()),
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), shorten(v))).collect())
        }
        other => other.clone(),
    }
}

pub fn history(env: &Env, a: &Args) -> Done {
    if let Some(id) = a.string("undo") {
        return undo(env, &id);
    }
    let readable: Vec<i64> = env.readable()?.iter().map(|(a, _)| a.id).collect();
    let lowest = match a.str("access") {
        None | Some("triage") => Level::Triage,
        Some("readOnly" | "read" | "read-only") => Level::ReadOnly,
        Some("send") => Level::Send,
        Some(other) => {
            return Err(Fail::Error(format!(
                "access is readOnly, triage or send, not {other}."
            )));
        }
    };
    let result = match a.str("result") {
        None => None,
        Some("success") => Some(Outcome::Success),
        Some("error") => Some(Outcome::Error),
        Some("blocked") => Some(Outcome::Blocked),
        Some(other) => {
            return Err(Fail::Error(format!(
                "result is success, error or blocked, not {other}."
            )));
        }
    };
    let from = a.str("from").map(|d| parse_time(d, false)).transpose()?;
    let to = a.str("to").map(|d| parse_time(d, true)).transpose()?;
    let (page, size) = a.paging(50)?;
    let verbose = a.flag("verbose");
    let now = now_ms();
    let mut entries: Vec<Entry> = env
        .log
        .read()
        .into_iter()
        .filter(|e| e.at >= now - KEEP_MS && e.visible(&readable))
        .filter(|e| e.access >= lowest)
        .filter(|e| result.is_none_or(|r| e.result == r))
        .filter(|e| from.is_none_or(|f| e.at >= f) && to.is_none_or(|t| e.at < t))
        .collect();
    entries.reverse();
    let total = entries.len();
    let page_entries: Vec<&Entry> = entries.iter().skip((page - 1) * size).take(size).collect();
    let rows: Vec<Value> = page_entries
        .iter()
        .map(|e| {
            let mut row = json!({
                "id": e.id,
                "at": super::iso(e.at),
                "agent": e.agent,
                "command": e.command,
                "access": e.access,
                "result": e.result,
                "summary": e.summary,
                "can_undo": !e.undo.is_empty() && !e.undone && e.result == Outcome::Success,
                "undone": e.undone,
            });
            if verbose {
                row["args"] = e.args.clone();
                row["targets"] = json!(e.targets);
                row["detail"] = json!(e.detail);
            }
            row
        })
        .collect();
    let mut out = String::new();
    if rows.is_empty() {
        out.push_str("Nothing in the history matches.");
    }
    for e in &page_entries {
        let what = if !e.summary.is_empty() {
            e.summary.clone()
        } else {
            e.detail.clone().unwrap_or_default()
        };
        let mut marks = Vec::new();
        if e.undone {
            marks.push("undone");
        } else if !e.undo.is_empty() && e.result == Outcome::Success {
            marks.push("can undo");
        }
        out.push_str(&format!(
            "{:>5}  {}  {}  {}  {}  {}{}\n",
            e.id,
            when(e.at),
            e.agent,
            e.command,
            match e.result {
                Outcome::Success => "success",
                Outcome::Error => "error",
                Outcome::Blocked => "blocked",
            },
            what,
            if marks.is_empty() {
                String::new()
            } else {
                format!(" ({})", marks.join(", "))
            }
        ));
        if verbose {
            out.push_str(&format!("       args: {}\n", e.args));
            if !e.targets.is_empty() {
                out.push_str(&format!("       targets: {}\n", e.targets.join(", ")));
            }
        }
    }
    if total > page * size {
        out.push_str(&format!("{} more: page {}.", total - page * size, page + 1));
    }
    Ok(Reply::new(
        json!({ "page": page, "page_size": size, "total": total, "entries": rows }),
        out.trim_end().to_owned(),
    ))
}

fn undo(env: &Env, id: &str) -> Done {
    let id: u64 = id
        .trim()
        .parse()
        .map_err(|_| Fail::Error(format!("{id} isn't a history ID.")))?;
    let readable: Vec<i64> = env.readable()?.iter().map(|(a, _)| a.id).collect();
    // Undoing needs triage where it acts, as doing did; the grants hold
    // still meanwhile.
    let (entry, skipped) = env.prefs.with_agent_access(|grants| {
        put_back_entry(
            env.engine,
            env.log,
            id,
            &|e| e.visible(&readable),
            &|store, accounts| {
                for &a in accounts {
                    if let Some(account) = store.account(a).map_err(db)? {
                        refuse(env.agent, grants, &account, Access::Triage, "undo that")?;
                    }
                }
                Ok(())
            },
        )
    })?;
    let mut out = format!("Undid entry {id}: {}.", entry.summary);
    for s in &skipped {
        out.push_str(&format!("\nLeft alone: {s}"));
    }
    let reply = Reply {
        effect: Some(Effect::Changed {
            summary: format!("undid “{}”", entry.summary),
        }),
        ..Reply::new(
            json!({ "undone": id, "summary": entry.summary, "left_alone": skipped }),
            out,
        )
    };
    Ok(reply.said(format!("undid “{}”", entry.summary)))
}

/// The user's Undo in Settings → Agents, which needs no grant. Returns
/// what was undone, and what was left alone because it changed since.
pub fn undo_for_user(
    engine: &spark_mail::Engine,
    log: &Log,
    id: u64,
) -> Result<(String, Vec<String>), String> {
    let (entry, skipped) = put_back_entry(engine, log, id, &|_| true, &|_, _| Ok(()))
        .map_err(|e| e.message().to_owned())?;
    Ok((entry.summary, skipped))
}

/// Whether steps may act in these accounts.
type Allowed<'a> = dyn Fn(&Store, &[i64]) -> Result<(), Fail> + 'a;

/// Puts back what entry `id` changed, once `allowed` lets every step act
/// in its accounts (all checked before any step runs), and marks it undone.
/// An entry `visible` hides doesn't exist for the caller.
fn put_back_entry(
    engine: &spark_mail::Engine,
    log: &Log,
    id: u64,
    visible: &dyn Fn(&Entry) -> bool,
    allowed: &Allowed,
) -> Result<(Entry, Vec<String>), Fail> {
    let _turn = log.last.lock().unwrap_or_else(|e| e.into_inner());
    let mut entries = log.read();
    let entry = entries
        .iter()
        .find(|e| e.id == id && visible(e))
        .cloned()
        .ok_or_else(|| Fail::Error(format!("There's no history entry {id}.")))?;
    if entry.undone {
        return Err(Fail::Error(format!("Entry {id} was undone already.")));
    }
    if entry.result != Outcome::Success || entry.undo.is_empty() {
        return Err(Fail::Error(format!(
            "Entry {id} changed nothing that can be put back: only actions, contact actions and drafts an agent wrote can be."
        )));
    }
    {
        let store = engine.store();
        let mut accounts = Vec::new();
        for step in &entry.undo {
            for a in step_accounts(&store, step).map_err(db)? {
                if !accounts.contains(&a) {
                    accounts.push(a);
                }
            }
        }
        allowed(&store, &accounts)?;
    }
    let mut skipped = Vec::new();
    for step in entry.undo.iter().rev() {
        let mut store = engine.store();
        match put_back(&mut store, step).map_err(db)? {
            Some(why) => skipped.push(why),
            None => {
                if let Step::Draft { id, .. } = step {
                    drop(store);
                    if let Err(e) = crate::mailbox::discard_saved_draft(engine, *id) {
                        skipped.push(format!("draft {id}: {e}"));
                    }
                }
            }
        }
    }
    if let Some(e) = entries.iter_mut().find(|e| e.id == id) {
        e.undone = true;
    }
    log.rewrite(&entries)
        .map_err(|e| Fail::Error(format!("The history couldn't be written: {e}")))?;
    Ok((entry, skipped))
}

/// The latest entries for Settings → Agents, newest first: what agents
/// changed, or with `reads` everything.
pub fn recent(log: &Log, reads: bool, limit: usize) -> Vec<Entry> {
    let now = now_ms();
    let mut entries: Vec<Entry> = log
        .read()
        .into_iter()
        .filter(|e| e.at >= now - KEEP_MS && (reads || e.access >= Level::Triage))
        .collect();
    entries.reverse();
    entries.truncate(limit);
    entries
}

/// The accounts a step acts in: its email's, or every account for a
/// sender's settings. None when what it changed is gone.
fn step_accounts(store: &Store, step: &Step) -> spark_store::Result<Vec<i64>> {
    Ok(match step {
        Step::Message { id, .. } | Step::Draft { id, .. } => store
            .message(*id)?
            .map(|m| m.account_id)
            .into_iter()
            .collect(),
        Step::Muted { conversation, .. } => store
            .messages()?
            .iter()
            .find(|m| m.conversation == *conversation)
            .map(|m| m.account_id)
            .into_iter()
            .collect(),
        Step::Reminder { id, .. } | Step::ReminderMoved { id, .. } => store
            .reminder(*id)?
            .map(|r| r.account_id)
            .into_iter()
            .collect(),
        Step::ReminderCleared { reminder } => vec![reminder.account_id],
        Step::Category { .. } | Step::Priority { .. } | Step::Gatekeeper { .. } => {
            store.accounts()?.iter().map(|a| a.id).collect()
        }
    })
}

/// A sender's or a domain's Gatekeeper decision now.
fn decision(store: &Store, screen: &Screen) -> spark_store::Result<Option<Decision>> {
    Ok(match screen {
        Screen::Sender(address) => store.sender_status(address)?.sender,
        Screen::Domain(domain) => store.sender_status(&format!("someone@{domain}"))?.domain,
    })
}

/// Puts back one step, only where what it changed is still as it left it;
/// says why when it leaves something alone.
fn put_back(store: &mut Store, step: &Step) -> spark_store::Result<Option<String>> {
    let now = now_ms();
    match step {
        Step::Message { id, before, after } => {
            let Some(m) = store.message(*id)? else {
                return Ok(Some(format!("email {id} is gone")));
            };
            if Snapshot::of(&m) != *after {
                return Ok(Some(format!("email {id} changed since")));
            }
            store.update(*id, &before.patch(), now)?;
        }
        Step::Muted {
            conversation,
            before,
        } => {
            let muted = store
                .messages()?
                .iter()
                .find(|m| m.conversation == *conversation)
                .map(|m| m.muted);
            match muted {
                None => return Ok(Some(format!("conversation {conversation} is gone"))),
                Some(muted) if muted == *before => {
                    return Ok(Some(format!("conversation {conversation} changed since")));
                }
                Some(_) => store.set_muted(*conversation, *before)?,
            }
        }
        Step::Category {
            address,
            before,
            after,
        } => {
            if store.sender_category(address)? != *after {
                return Ok(Some(format!("{address}'s category changed since")));
            }
            store.set_sender_category(address, *before)?;
        }
        Step::Priority { undo } => {
            // The agent made these Priority (or not); put back only those
            // still as it left them.
            let set = !undo.priority;
            let now_priority = store.priority_senders()?;
            let senders: Vec<String> = undo
                .senders
                .iter()
                .filter(|s| now_priority.iter().any(|p| p.eq_ignore_ascii_case(s)) == set)
                .cloned()
                .collect();
            let mut messages = Vec::new();
            for &id in &undo.messages {
                if store.message(id)?.is_some_and(|m| m.priority == set) {
                    messages.push(id);
                }
            }
            let left = undo.senders.len() - senders.len();
            store.undo_prioritize(&PriorityUndo {
                senders,
                messages,
                priority: undo.priority,
            })?;
            if left > 0 {
                return Ok(Some(format!("{left} Priority sender(s) changed since")));
            }
        }
        Step::Gatekeeper { undo, set } => {
            let mut still = Vec::new();
            let mut left = Vec::new();
            for change in undo {
                let made = set
                    .iter()
                    .find(|c| c.screen == change.screen)
                    .map(|c| c.decision);
                let unchanged = match made {
                    Some(made) => decision(store, &change.screen)? == made,
                    // Written before the history kept what was set.
                    None => true,
                };
                if unchanged {
                    still.push(change.clone());
                } else {
                    left.push(match &change.screen {
                        Screen::Sender(s) => s.clone(),
                        Screen::Domain(d) => d.clone(),
                    });
                }
            }
            store.decide(&still, now)?;
            if !left.is_empty() {
                return Ok(Some(format!("{} decided again since", left.join(", "))));
            }
        }
        Step::Draft { id, print } => {
            if !store.is_draft(*id)? {
                return Ok(Some(format!("draft {id} is gone")));
            }
            if draft_print(store, *id)? != *print {
                return Ok(Some(format!("draft {id} was edited since")));
            }
            if store.draft_held(*id)? || store.draft_reserved(*id)? {
                return Ok(Some(format!("draft {id} is being sent")));
            }
            // Deleted by the caller, as Discard does, with the store let go.
            return Ok(None);
        }
        Step::Reminder {
            id,
            at,
            account,
            email,
        } => match store.reminder(*id)? {
            None => return Ok(Some(format!("reminder {id} is gone"))),
            Some(r) if r.account_id != *account || store.reminder_email(*id)? != *email => {
                return Ok(Some(format!("reminder {id} is gone")));
            }
            Some(r) if r.due || at.is_some_and(|at| r.remind_at != at) => {
                return Ok(Some(format!("reminder {id} changed since")));
            }
            Some(_) => store.set_reminder(*id, None)?,
        },
        Step::ReminderCleared { reminder } => {
            if store.reminder(reminder.id)?.is_some() || !store.restore_reminder(reminder)? {
                return Ok(Some(format!("reminder {} couldn't come back", reminder.id)));
            }
        }
        Step::ReminderMoved {
            id,
            from,
            to,
            account,
            email,
        } => match store.reminder(*id)? {
            None => return Ok(Some(format!("reminder {id} is gone"))),
            Some(r) if r.account_id != *account || store.reminder_email(*id)? != *email => {
                return Ok(Some(format!("reminder {id} is gone")));
            }
            Some(r) if r.due || r.remind_at != *to => {
                return Ok(Some(format!("reminder {id} changed since")));
            }
            Some(_) => store.set_reminder(*id, Some(*from))?,
        },
    }
    Ok(None)
}
