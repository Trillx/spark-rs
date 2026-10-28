//! The commands agents run through `spark-cli`, named and shaped like the
//! Spark CLI's (`accounts`, `emails`, `thread`, `draft`, `action`, …) with
//! the same flags and MCP parameters, so agents and skills written for
//! Spark work here too. Each needs read, triage or send access to the
//! accounts it touches, as the user set it in Settings → Agents, and each
//! run goes into the history (`history.rs`), which can undo triage.
//!
//! A command answers with JSON for scripts (`spark-cli --json`) and text
//! for reading (`text.rs`). Store work happens under the agents' grants, so
//! a grant revoked meanwhile can't let a change through; network work
//! (bodies, attachments, calendars) happens between, with nothing held.

mod calendar;
mod catalog;
mod compose;
mod filter;
mod history;
mod mail;
mod text;
mod triage;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, NaiveTime, SecondsFormat, TimeZone};
use serde_json::{Value, json};
use spark_mail::Engine;
use spark_store::{Account, Store, now_ms};

pub use catalog::catalog;
pub use history::{HISTORY_FILE, Log, recent, undo_for_user};

use super::Access;
use super::tools::Effect;
use crate::prefs::PrefsState;
use history::{Level, Step};

/// Longer text in a message is cut here.
const MAX_TEXT: usize = 50_000;

/// What a command runs with.
pub struct Env<'a> {
    pub engine: &'a Engine,
    /// Holds the grants: what agents may do in each account.
    pub prefs: &'a PrefsState,
    /// The agent's name, e.g. "Codex", for refusals and the history.
    pub agent: &'a str,
    /// Where `attachment` leaves copies of files for spark-cli to read.
    pub files: &'a Path,
    pub log: &'a Log,
    /// Wakes an account's worker, to send what's due at once.
    pub nudge: &'a (dyn Fn(i64) + Send + Sync),
}

/// A command's answer, as `tools/call` returns it.
#[derive(Debug)]
pub struct Outcome {
    /// For reading; the error's message when `is_error`.
    pub text: String,
    /// For scripts: the result, or `{"error": {"message": …}}`.
    pub value: Value,
    pub is_error: bool,
    /// What the UI should hear about.
    pub effect: Option<Effect>,
}

/// Why a command didn't run.
#[derive(Debug, PartialEq)]
pub enum Fail {
    /// The user didn't allow it: the history says "blocked".
    Blocked(String),
    Error(String),
}

impl Fail {
    fn message(&self) -> &str {
        match self {
            Self::Blocked(m) | Self::Error(m) => m,
        }
    }
}

impl From<String> for Fail {
    fn from(message: String) -> Self {
        Self::Error(message)
    }
}

impl From<&str> for Fail {
    fn from(message: &str) -> Self {
        Self::Error(message.to_owned())
    }
}

fn db(e: spark_store::Error) -> Fail {
    Fail::Error(format!("spark-rs couldn't read its mail database: {e}"))
}

/// What a command did, before it's rendered.
#[derive(Debug, Default)]
pub struct Reply {
    pub value: Value,
    /// The text for reading; empty to render `value` generically.
    pub text: String,
    pub effect: Option<Effect>,
    /// For the history: what it touched, in a few words, and how to undo it.
    pub summary: String,
    pub targets: Vec<String>,
    pub undo: Vec<Step>,
}

impl Reply {
    fn new(value: Value, text: String) -> Self {
        Self {
            value,
            text,
            ..Self::default()
        }
    }

    /// With what the history says the agent did.
    fn said(mut self, summary: impl Into<String>) -> Self {
        self.summary = summary.into();
        self
    }
}

type Done = Result<Reply, Fail>;

/// The commands this module answers; others are the older tools'.
pub fn knows(name: &str) -> bool {
    matches!(
        name,
        "accounts"
            | "folders"
            | "emails"
            | "search"
            | "thread"
            | "attachment"
            | "draft"
            | "templates"
            | "template"
            | "events"
            | "event"
            | "availability"
            | "contacts"
            | "contact"
            | "action"
            | "contact-action"
            | "history"
    )
}

/// Runs one command and writes it to the history.
pub async fn run(env: &Env<'_>, name: &str, args: &Value) -> Outcome {
    let args = match args {
        Value::Null => json!({}),
        Value::Object(_) => args.clone(),
        _ => {
            return failed(Fail::Error(
                "A command's arguments are a JSON object.".into(),
            ));
        }
    };
    let a = Args(&args);
    let started = now_ms();
    // What the entry could hold comes from these: agents see it only while
    // they can read them all.
    let shared: Vec<i64> = env
        .grants()
        .into_iter()
        .filter(|(_, access)| *access >= Access::Read)
        .map(|(id, _)| id)
        .collect();
    let result = match name {
        "accounts" => mail::accounts(env),
        "folders" => mail::folders(env, &a),
        "emails" => mail::emails(env, &a),
        "search" => mail::search(env, &a),
        "thread" => mail::thread(env, &a).await,
        "attachment" => mail::attachment(env, &a).await,
        "draft" => compose::draft(env, &a).await,
        "templates" => compose::templates(env, &a),
        "template" => compose::template(env, &a),
        "events" => calendar::events(env, &a).await,
        "event" => calendar::event(env, &a).await,
        "availability" => calendar::availability(env, &a).await,
        "contacts" => triage::contacts(env, &a),
        "contact" => triage::contact(env, &a),
        "action" => triage::action(env, &a).await,
        "contact-action" => triage::contact_action(env, &a),
        "history" => history::history(env, &a),
        other => Err(Fail::Error(format!("spark-rs has no command {other}."))),
    };
    if name != "history" || a.str("undo").is_some() {
        let run = history::Run {
            agent: env.agent,
            command: name,
            args: &args,
            access: level(name, &a),
            at: started,
            accounts: shared,
        };
        env.log.record(run, &result);
    }
    match result {
        Ok(reply) => {
            let text = if reply.text.is_empty() {
                text::generic(&reply.value)
            } else {
                reply.text
            };
            Outcome {
                text,
                value: reply.value,
                is_error: false,
                effect: reply.effect,
            }
        }
        Err(fail) => failed(fail),
    }
}

fn failed(fail: Fail) -> Outcome {
    let message = fail.message().to_owned();
    Outcome {
        value: json!({ "error": { "message": message } }),
        text: message,
        is_error: true,
        effect: None,
    }
}

/// The access a command needs, for the history's filter.
fn level(name: &str, a: &Args) -> Level {
    match name {
        "draft" if a.str("mode") == Some("signatures") => Level::ReadOnly,
        "action" if matches!(a.str("action_name"), Some("send" | "unschedule")) => Level::Send,
        "history" if a.str("undo").is_some() => Level::Triage,
        "draft" | "action" | "contact-action" => Level::Triage,
        "event" => Level::Send,
        _ => Level::ReadOnly,
    }
}

// ---- Access ----

impl Env<'_> {
    fn grants(&self) -> BTreeMap<i64, Access> {
        self.prefs.with_agent_access(Clone::clone)
    }

    fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        self.engine.store()
    }

    /// The accounts agents may at least read, in order, with their access.
    fn readable(&self) -> Result<Vec<(Account, Access)>, Fail> {
        let grants = self.grants();
        let accounts = self.store().accounts().map_err(db)?;
        let shared: Vec<_> = accounts
            .into_iter()
            .filter_map(|a| {
                let access = grants.get(&a.id).copied().unwrap_or_default();
                (access >= Access::Read).then_some((a, access))
            })
            .collect();
        if shared.is_empty() {
            return Err(Fail::Blocked(
                "The user hasn't shared any account with agents. They can in spark-rs > Settings > Agents."
                    .into(),
            ));
        }
        Ok(shared)
    }

    /// Runs `work` with the store while the grants can't change, once every
    /// account in `accounts` has `needed`.
    fn guarded<T>(
        &self,
        accounts: &[i64],
        needed: Access,
        what: &str,
        work: impl FnOnce(&mut Store) -> Result<T, Fail>,
    ) -> Result<T, Fail> {
        self.prefs.with_agent_access(|grants| {
            let mut store = self.store();
            for &id in accounts {
                let account = store
                    .account(id)
                    .map_err(db)?
                    .ok_or_else(|| Fail::Error(format!("There's no account {id}.")))?;
                refuse(self.agent, grants, &account, needed, what)?;
            }
            work(&mut store)
        })
    }

    /// Fails unless `account` has `needed` now.
    fn need(&self, account: &Account, needed: Access, what: &str) -> Result<(), Fail> {
        self.prefs
            .with_agent_access(|grants| refuse(self.agent, grants, account, needed, what))
    }

    /// The account with this address (or an alias, or its id), shared with agents.
    fn account_named(&self, name: &str) -> Result<Account, Fail> {
        let wanted = name.trim().to_lowercase();
        let accounts = self.store().accounts().map_err(db)?;
        let found = accounts
            .into_iter()
            .find(|a| a.email.to_lowercase() == wanted || a.id.to_string() == wanted);
        match found {
            Some(a) if self.access(a.id) >= Access::Read => Ok(a),
            _ => Err(Fail::Error(format!(
                "There's no account {name} shared with agents. `accounts` lists them."
            ))),
        }
    }

    fn access(&self, account: i64) -> Access {
        self.prefs
            .with_agent_access(|g| g.get(&account).copied().unwrap_or_default())
    }

    fn account(&self, id: i64) -> Result<Account, Fail> {
        self.store()
            .account(id)
            .map_err(db)?
            .ok_or_else(|| Fail::Error(format!("There's no account {id}.")))
    }

    /// A stored message the agent may read.
    fn message(&self, id: i64) -> Result<(spark_store::Message, Account), Fail> {
        let m = self
            .store()
            .message(id)
            .map_err(db)?
            .ok_or_else(|| no_message(id))?;
        let account = self.account(m.account_id)?;
        self.need(&account, Access::Read, "read mail")?;
        Ok((m, account))
    }
}

/// Everything waiting in the outbox, soonest first: scheduled with Send
/// Later, in its undo window, or failing on the network and tried again.
fn outbox(env: &Env) -> Result<Vec<spark_mail::engine::WaitingSend>, Fail> {
    let failing: std::collections::HashMap<i64, String> = env
        .engine
        .waiting_sends()
        .map_err(|e| Fail::Error(e.to_string()))?
        .into_iter()
        .filter_map(|w| Some((w.id, w.failed?)))
        .collect();
    let mut sends: Vec<_> = env
        .store()
        .outbox_sends()
        .map_err(db)?
        .into_iter()
        .filter_map(|s| {
            Some(spark_mail::engine::WaitingSend {
                draft: serde_json::from_str(&s.draft).ok()?,
                failed: failing.get(&s.id).cloned(),
                id: s.id,
                account_id: s.account_id,
                send_at: s.send_at,
                sending: s.sending,
            })
        })
        .collect();
    sends.sort_by_key(|w| (w.send_at, w.id));
    Ok(sends)
}

fn refuse(
    agent: &str,
    grants: &BTreeMap<i64, Access>,
    account: &Account,
    needed: Access,
    what: &str,
) -> Result<(), Fail> {
    let has = grants.get(&account.id).copied().unwrap_or_default();
    if has >= needed {
        return Ok(());
    }
    Err(Fail::Blocked(format!(
        "{agent} can't {what} in {}: the user allowed {} there, and this needs {}. They can change it in spark-rs > Settings > Agents.",
        account.email,
        has.label(),
        needed.label()
    )))
}

fn no_message(id: i64) -> Fail {
    Fail::Error(format!("There's no email {id}."))
}

// ---- Arguments ----

/// A command's arguments, read leniently: MCP clients send ids as strings
/// or numbers and lists as arrays, a shell sends single values.
pub struct Args<'a>(pub &'a Value);

impl Args<'_> {
    /// A string, trimmed; `None` when absent or empty.
    fn str(&self, key: &str) -> Option<&str> {
        self.0[key]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    fn string(&self, key: &str) -> Option<String> {
        match &self.0[key] {
            Value::Number(n) => Some(n.to_string()),
            _ => self.str(key).map(str::to_owned),
        }
    }

    /// Text kept as written (a body), `None` when absent.
    fn raw(&self, key: &str) -> Option<&str> {
        self.0[key].as_str()
    }

    fn flag(&self, key: &str) -> bool {
        match &self.0[key] {
            Value::Bool(b) => *b,
            Value::String(s) => matches!(s.trim(), "true" | "1" | "yes"),
            _ => false,
        }
    }

    /// Every value given for `key`: an array, or one value.
    fn list(&self, key: &str) -> Vec<String> {
        let one = |v: &Value| match v {
            Value::String(s) => Some(s.trim().to_owned()).filter(|s| !s.is_empty()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        };
        match &self.0[key] {
            Value::Array(items) => items.iter().filter_map(one).collect(),
            other => one(other).into_iter().collect(),
        }
    }

    /// `list`, with each value split at commas too (`a@x.com,b@x.com`).
    fn addresses(&self, key: &str) -> Vec<String> {
        self.list(key)
            .iter()
            .flat_map(|v| v.split([',', ';']))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    }

    fn int(&self, key: &str) -> Result<Option<i64>, Fail> {
        match &self.0[key] {
            Value::Null => Ok(None),
            Value::Number(n) => n
                .as_i64()
                .map(Some)
                .ok_or_else(|| Fail::Error(format!("{key} must be a whole number."))),
            Value::String(s) if s.trim().is_empty() => Ok(None),
            Value::String(s) => s
                .trim()
                .parse()
                .map(Some)
                .map_err(|_| Fail::Error(format!("{key} must be a whole number, not {s}."))),
            _ => Err(Fail::Error(format!("{key} must be a whole number."))),
        }
    }

    /// A page number and size: `page` from 1, `page_size` (or `limit`).
    fn paging(&self, default: usize) -> Result<(usize, usize), Fail> {
        let page = self.int("page")?.unwrap_or(1);
        if page < 1 {
            return Err("page starts at 1.".into());
        }
        let size = match self.int("page_size")? {
            Some(n) => Some(n),
            None => self.int("limit")?,
        }
        .unwrap_or(default as i64);
        if !(1..=1000).contains(&size) {
            return Err("page_size must be from 1 to 1000.".into());
        }
        Ok((page as usize, size as usize))
    }
}

/// A message id as agents write it: `1114`, or a number in a string.
fn message_id(text: &str) -> Result<i64, Fail> {
    let text = text.trim();
    text.parse().ok().filter(|&id: &i64| id > 0).ok_or_else(|| {
        Fail::Error(format!(
            "{text} isn't an email ID. IDs come from emails, search or thread."
        ))
    })
}

// ---- Times ----

/// A local time as agents write it: `2026-07-01`, `01/07/2026`,
/// `2026/07/01`, `2026-07-01T12:00`, with seconds, or RFC 3339 with an
/// offset. A date alone is its start, or with `end_of_day` the start of the
/// next day.
fn parse_time(text: &str, end_of_day: bool) -> Result<i64, Fail> {
    let text = text.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(text) {
        return Ok(t.timestamp_millis());
    }
    for format in [
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%d %H:%M:%S",
    ] {
        if let Ok(t) = NaiveDateTime::parse_from_str(text, format) {
            return local_ms(t);
        }
    }
    for format in ["%Y-%m-%d", "%d/%m/%Y", "%Y/%m/%d"] {
        if let Ok(day) = NaiveDate::parse_from_str(text, format) {
            let day = if end_of_day {
                day.succ_opt().unwrap_or(day)
            } else {
                day
            };
            return local_ms(day.and_time(NaiveTime::MIN));
        }
    }
    Err(Fail::Error(format!(
        "{text} isn't a date or time. Write yyyy-MM-dd, dd/MM/yyyy or yyyy-MM-ddTHH:mm."
    )))
}

/// Whether `text` names a whole day rather than a time.
fn is_date(text: &str) -> bool {
    let text = text.trim();
    ["%Y-%m-%d", "%d/%m/%Y", "%Y/%m/%d"]
        .iter()
        .any(|f| NaiveDate::parse_from_str(text, f).is_ok())
}

fn local_ms(t: NaiveDateTime) -> Result<i64, Fail> {
    Local
        .from_local_datetime(&t)
        .earliest()
        .map(|t| t.timestamp_millis())
        .ok_or_else(|| Fail::Error(format!("{t} doesn't exist on this computer's clock.")))
}

/// ISO 8601 in the computer's time zone, for JSON.
fn iso(ms: i64) -> String {
    match Local.timestamp_millis_opt(ms).single() {
        Some(t) => t.to_rfc3339_opts(SecondsFormat::Secs, false),
        None => String::new(),
    }
}

/// `2026-10-27 09:12`, for reading.
fn when(ms: i64) -> String {
    match Local.timestamp_millis_opt(ms).single() {
        Some(t) => t.format("%Y-%m-%d %H:%M").to_string(),
        None => String::new(),
    }
}

/// Cuts `text` to `max` bytes at a character boundary, saying so.
fn cut(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut at = max;
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        text.truncate(at);
        text.push_str("\n\n[spark-rs cut the rest of this long text.]");
    }
    text
}

fn count(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// "Archived 3 emails", from "archived 3 emails".
fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
