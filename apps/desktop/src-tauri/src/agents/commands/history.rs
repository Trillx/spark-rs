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
use spark_store::{Category, Change, Message, MessageState, Patch, PriorityUndo, Store, now_ms};

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
    Gatekeeper {
        undo: Vec<Change>,
    },
    /// A draft the agent wrote, as saved then.
    Draft {
        id: i64,
        saved_at: Option<i64>,
    },
    Reminder {
        id: i64,
    },
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
    pub fn record(
        &self,
        agent: &str,
        command: &str,
        args: &Value,
        access: Level,
        at: i64,
        result: &Result<Reply, Fail>,
    ) {
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
    env.readable()?;
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
        .filter(|e| e.at >= now - KEEP_MS)
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
    // Undoing needs triage where it acts, as doing did; the grants hold
    // still meanwhile.
    let (entry, skipped) = env.prefs.with_agent_access(|grants| {
        put_back_entry(env.engine, env.log, id, &|store, account| {
            let accounts = match account {
                Some(a) => vec![a],
                None => store.accounts().map_err(db)?.iter().map(|a| a.id).collect(),
            };
            for a in accounts {
                if let Some(account) = store.account(a).map_err(db)? {
                    refuse(env.agent, grants, &account, Access::Triage, "undo that")?;
                }
            }
            Ok(())
        })
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
    let (entry, skipped) =
        put_back_entry(engine, log, id, &|_, _| Ok(())).map_err(|e| e.message().to_owned())?;
    Ok((entry.summary, skipped))
}

/// Whether a step may act in an account (`None`: every account).
type Allowed<'a> = dyn Fn(&Store, Option<i64>) -> Result<(), Fail> + 'a;

/// Puts back what entry `id` changed, once `allowed` lets each step act in
/// its account, and marks it undone.
fn put_back_entry(
    engine: &spark_mail::Engine,
    log: &Log,
    id: u64,
    allowed: &Allowed,
) -> Result<(Entry, Vec<String>), Fail> {
    let _turn = log.last.lock().unwrap_or_else(|e| e.into_inner());
    let mut entries = log.read();
    let entry = entries
        .iter()
        .find(|e| e.id == id)
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
    let mut skipped = Vec::new();
    for step in entry.undo.iter().rev() {
        let mut store = engine.store();
        allowed(&store, step_account(&store, step).map_err(db)?)?;
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

/// The account a step acts in; `None` for settings of every account.
fn step_account(store: &Store, step: &Step) -> spark_store::Result<Option<i64>> {
    Ok(match step {
        Step::Message { id, .. } | Step::Draft { id, .. } => {
            store.message(*id)?.map(|m| m.account_id)
        }
        Step::Muted { conversation, .. } => store
            .messages()?
            .iter()
            .find(|m| m.conversation == *conversation)
            .map(|m| m.account_id),
        Step::Reminder { .. }
        | Step::Category { .. }
        | Step::Priority { .. }
        | Step::Gatekeeper { .. } => None,
    })
}

/// Puts back one step; says why if it's left alone because what it changed
/// has changed since.
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
        } => store.set_muted(*conversation, *before)?,
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
        Step::Priority { undo } => store.undo_prioritize(undo)?,
        Step::Gatekeeper { undo } => {
            store.decide(undo, now)?;
        }
        Step::Draft { id, saved_at } => {
            if !store.is_draft(*id)? {
                return Ok(Some(format!("draft {id} is gone")));
            }
            if store.draft_saved_at(*id)? != *saved_at {
                return Ok(Some(format!("draft {id} was edited since")));
            }
            if store.draft_held(*id)? || store.draft_reserved(*id)? {
                return Ok(Some(format!("draft {id} is being sent")));
            }
            // Deleted by the caller, as Discard does, with the store let go.
            return Ok(None);
        }
        Step::Reminder { id } => store.set_reminder(*id, None)?,
    }
    Ok(None)
}
