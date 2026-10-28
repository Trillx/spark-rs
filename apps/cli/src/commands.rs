//! Spark's CLI commands (`accounts`, `emails`, `thread`, `draft`, `action`,
//! …) with its flags. Each becomes the MCP tool of the same name, run by
//! spark-rs; the result prints as text, or with `--json` as JSON.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde_json::{Map, Value, json};

use crate::agent::Link;

/// Every skill, by name, from `skills/<name>/SKILL.md` (see `build.rs`).
const SKILLS: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/skills.rs"));
/// The skill the others build on: the commands.
const BASE: &str = "use-spark-rs";
/// The others' names start with this.
const PREFIX: &str = "spark-rs-";

/// A skill's text by name (`inbox-triage` or `spark-rs-inbox-triage`), its
/// version spark-cli's.
fn skill_text(name: &str) -> Option<(&'static str, String)> {
    let name = name.trim();
    SKILLS
        .iter()
        .find(|(n, _)| *n == name || n.strip_prefix(PREFIX) == Some(name))
        .map(|(n, text)| (*n, text.replace("{{version}}", env!("CARGO_PKG_VERSION"))))
}

/// The frontmatter's `description`, on one line.
fn description(text: &str) -> String {
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines().skip(1) {
        if line == "---" {
            break;
        }
        if let Some(rest) = line.strip_prefix("description:") {
            inside = true;
            let rest = rest.trim().trim_start_matches(">-").trim();
            if !rest.is_empty() {
                out.push(rest.to_owned());
            }
        } else if inside && line.starts_with("  ") {
            out.push(line.trim().to_owned());
        } else if inside {
            break;
        }
    }
    out.join(" ")
}

#[derive(Args, Default)]
pub struct Paging {
    /// Page number, from 1.
    #[arg(long)]
    page: Option<u32>,
    /// How many per page (default 50).
    #[arg(long, visible_alias = "limit")]
    page_size: Option<u32>,
}

#[derive(Args, Default)]
pub struct Range {
    /// From now to the end of today (the default).
    #[arg(long, conflicts_with_all = ["tomorrow", "week", "start", "end"])]
    today: bool,
    /// All of tomorrow.
    #[arg(long, conflicts_with_all = ["week", "start", "end"])]
    tomorrow: bool,
    /// From now to the end of this week.
    #[arg(long, conflicts_with_all = ["start", "end"])]
    week: bool,
    /// Start: yyyy-MM-dd, dd/MM/yyyy or yyyy-MM-ddTHH:mm.
    #[arg(long)]
    start: Option<String>,
    /// End; a date includes that day.
    #[arg(long)]
    end: Option<String>,
}

#[derive(Subcommand)]
pub enum Mail {
    /// List the accounts shared with agents, their access, calendars and signatures.
    Accounts,
    /// List folders and labels with message counts and their identifiers.
    Folders {
        /// Only this account.
        account: Option<String>,
    },
    /// List conversations in a folder (the Unified Inbox by default).
    ///
    /// Folders: Inbox, New Senders, Pinned, Snoozed, Set Aside, Done, Sent,
    /// Drafts, Spam, Trash, Blocked, Scheduled; user@example.com (its
    /// inbox); user@example.com:Archive or user@example.com:Projects.
    ///
    /// Filter operators: from: to: cc: subject: before:yyyy/MM/dd after:
    /// newer_than:7d older_than: has:attachment|document|spreadsheet|
    /// presentation|reminder is:unread|read|pinned|starred|unreplied|
    /// priority|muted|snoozed category:priority|personal|notification|
    /// newsletter|invitation|invitation_response filename:; -op: excludes.
    Emails {
        /// Folders to list.
        folder: Vec<String>,
        /// Gmail-style filter, e.g. "from:ana is:unread newer_than:7d".
        #[arg(long)]
        filter: Option<String>,
        #[command(flatten)]
        paging: Paging,
        /// ascending or descending (newest first, the default).
        #[arg(long)]
        order: Option<String>,
        /// Only mail from new senders waiting at the Gatekeeper.
        #[arg(long)]
        new_senders: bool,
    },
    /// Search every folder by topic (best 20 with their text), or list by --filter.
    Search {
        /// What to search for. Omit to list emails matching --filter in all folders.
        query: Option<String>,
        #[arg(long)]
        filter: Option<String>,
        /// Where: user@example.com, user@example.com:Archive or a folder name.
        #[arg(long = "in")]
        scope: Option<String>,
        #[command(flatten)]
        paging: Paging,
        #[arg(long)]
        order: Option<String>,
    },
    /// Print a whole conversation: headers, text and attachments.
    Thread {
        /// An email ID from emails or search.
        message_id: String,
        /// Download the attachments too.
        #[arg(long)]
        download_attachments: bool,
    },
    /// Download an attachment (an ID like 1114/2 from thread) and print where it is.
    Attachment {
        id: String,
        /// Write the file's bytes to stdout instead, e.g. `attachment 1114/2 --stream > file.pdf`.
        #[arg(long, conflicts_with = "json")]
        stream: bool,
    },
    /// Write, change or delete a draft; `draft signatures` lists signatures.
    ///
    /// The body is markdown. The account's signature is added below it, so
    /// don't write a sign-off of your own. `action send <ID>` sends it.
    Draft {
        /// `signatures`: list the signature each account adds.
        mode: Option<String>,
        /// A recipient, e.g. "Ana Ruiz <ana@example.com>"; repeat for more.
        #[arg(long)]
        to: Vec<String>,
        /// A Cc recipient; repeat for more.
        #[arg(long)]
        cc: Vec<String>,
        /// A Bcc recipient; repeat for more.
        #[arg(long)]
        bcc: Vec<String>,
        /// The subject; replies and forwards make one.
        #[arg(long)]
        subject: Option<String>,
        /// The text, in markdown.
        #[arg(long)]
        body: Option<String>,
        /// The account to write from.
        #[arg(long)]
        account: Option<String>,
        /// A draft's ID, to change it.
        #[arg(long)]
        edit: Option<String>,
        /// An email's ID, to answer its sender.
        #[arg(long, conflicts_with = "reply_all")]
        reply_to: Option<String>,
        /// An email's ID, to answer everyone on it.
        #[arg(long)]
        reply_all: Option<String>,
        /// An email's ID, to forward it with its files.
        #[arg(long)]
        forward: Option<String>,
        /// A draft's ID, to delete it for good (drafts have no Trash).
        #[arg(long)]
        delete: Option<String>,
        /// A file to attach, by its absolute path.
        #[arg(long)]
        attach: Vec<PathBuf>,
        /// An attachment ID from thread (1114/2), to send it again.
        #[arg(long)]
        attach_id: Vec<String>,
        /// Attach the bytes piped to stdin as a file with this name.
        #[arg(long, value_name = "NAME")]
        attach_stream: Option<String>,
        /// A template's ID or name.
        #[arg(long)]
        template: Option<String>,
        /// A template placeholder's value, "Name=value".
        #[arg(long)]
        placeholder: Vec<String>,
        /// Leave out the account's signature.
        #[arg(long)]
        no_signature: bool,
    },
    /// List the email templates.
    Templates {
        #[arg(long)]
        personal: bool,
        #[arg(long)]
        team: Option<String>,
        #[command(flatten)]
        paging: Paging,
    },
    /// Show one template with its placeholders.
    Template {
        /// Its ID or name.
        #[arg(value_name = "REF")]
        reference: String,
    },
    /// List calendar events (today's remaining ones by default).
    Events {
        #[command(flatten)]
        range: Range,
        /// user@example.com, or user@example.com:Calendar.
        #[arg(long = "in")]
        scope: Option<String>,
    },
    /// Make, change, delete or answer an event (needs send access).
    ///
    /// event create --title "Sync" --start 2026-07-01T12:00 --add ana@example.com;
    /// event update e42 --location "Room 7"; event delete e42;
    /// event rsvp e42 accept, or with an invitation email's ID.
    Event {
        /// create, update, delete or rsvp.
        mode: String,
        /// An event ID (e42), or for rsvp an invitation email's ID.
        event_id: Option<String>,
        /// For rsvp: accept, decline or maybe.
        status: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        start: Option<String>,
        #[arg(long)]
        end: Option<String>,
        #[arg(long)]
        all_day: bool,
        #[arg(long)]
        description: Option<String>,
        /// 300s for five minutes before, or a time.
        #[arg(long)]
        alerts: Option<String>,
        #[arg(long)]
        location: Option<String>,
        /// teams (or auto).
        #[arg(long)]
        video_conference: Option<String>,
        /// busy (the default), free, tentative, oof or working-elsewhere.
        #[arg(long)]
        show_as: Option<String>,
        /// user@example.com or user@example.com:Calendar, for create.
        #[arg(long)]
        calendar: Option<String>,
        /// People to invite; repeat or comma-separate.
        #[arg(long)]
        add: Vec<String>,
        /// People to uninvite.
        #[arg(long)]
        remove: Vec<String>,
    },
    /// Find free time between 08:00 and 20:00 on weekdays.
    Availability {
        #[command(flatten)]
        range: Range,
        /// Other people, comma-separated.
        #[arg(long)]
        attendees: Option<String>,
    },
    /// Find people by name or address, or list the ones you write to most.
    Contacts {
        query: Option<String>,
        /// 1 to 1000 (default 25).
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Show how spark-rs treats a sender.
    Contact { email: String },
    /// Act on emails: archive, pin, snooze, moveToFolder, send, ….
    ///
    /// pin unpin mute unmute snooze(--date) unsnooze changeReminder(--date)
    /// clearReminder setAside archive markAsDone markAsUndone moveToInbox
    /// moveToTrash moveToFolder(--folder) markAsSeen markAsUnseen markAsSpam
    /// markThreadAsPriority unmarkThreadAsPriority unsubscribe
    /// changeCategoryPersonal changeCategoryNotification
    /// changeCategoryNewsletters; with send access send [--date] and
    /// unschedule.
    Action {
        action_name: String,
        /// Email IDs (or scheduled IDs such as s12 for send and unschedule).
        #[arg(required = true)]
        message_ids: Vec<String>,
        #[arg(long)]
        date: Option<String>,
        #[arg(long)]
        folder: Option<String>,
        #[arg(long)]
        team: Option<String>,
        #[arg(long)]
        user: Vec<String>,
        #[arg(long)]
        assignee: Option<String>,
        #[arg(long)]
        comment: Option<String>,
    },
    /// Change how senders are treated: category, Priority, accept or block.
    ContactAction {
        action_name: String,
        #[arg(required = true)]
        emails: Vec<String>,
    },
    /// List what agents did; `history undo <ID>` puts an entry back.
    History {
        #[command(subcommand)]
        undo: Option<Undo>,
        #[command(flatten)]
        paging: Paging,
        /// Each entry's arguments and targets too.
        #[arg(long)]
        verbose: bool,
        /// The lowest access an entry needed: readOnly, triage (default) or send.
        #[arg(long)]
        access: Option<String>,
        /// success, error or blocked.
        #[arg(long)]
        result: Option<String>,
        /// On or after this date (yyyy-MM-dd).
        #[arg(long)]
        from: Option<String>,
        /// On or before this date.
        #[arg(long)]
        to: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum Undo {
    /// Put back what an entry changed.
    Undo {
        /// The entry's ID, from history.
        id: u64,
    },
}

/// Arguments as a JSON object, leaving out what wasn't given.
#[derive(Default)]
struct Params(Map<String, Value>);

impl Params {
    fn str(mut self, key: &str, value: Option<String>) -> Self {
        if let Some(v) = value {
            self.0.insert(key.into(), Value::String(v));
        }
        self
    }

    fn list(mut self, key: &str, values: Vec<String>) -> Self {
        if !values.is_empty() {
            self.0.insert(key.into(), json!(values));
        }
        self
    }

    fn flag(mut self, key: &str, on: bool) -> Self {
        if on {
            self.0.insert(key.into(), Value::Bool(true));
        }
        self
    }

    fn int(mut self, key: &str, value: Option<u32>) -> Self {
        if let Some(n) = value {
            self.0.insert(key.into(), json!(n));
        }
        self
    }

    fn paging(self, p: Paging) -> Self {
        self.int("page", p.page).int("page_size", p.page_size)
    }

    fn range(self, r: Range) -> Self {
        self.flag("today", r.today)
            .flag("tomorrow", r.tomorrow)
            .flag("week", r.week)
            .str("start", r.start)
            .str("end", r.end)
    }
}

pub async fn run(mail: Mail, json: bool) -> Result<()> {
    let p = Params::default();
    let mut stream = false;
    let mut upload: Option<Upload> = None;
    let (tool, params) = match mail {
        Mail::Accounts => ("accounts", p),
        Mail::Folders { account } => ("folders", p.str("account", account)),
        Mail::Emails {
            folder,
            filter,
            paging,
            order,
            new_senders,
        } => (
            "emails",
            p.list("folder", folder)
                .str("filter", filter)
                .paging(paging)
                .str("order", order)
                .flag("new_senders", new_senders),
        ),
        Mail::Search {
            query,
            filter,
            scope,
            paging,
            order,
        } => (
            "search",
            p.str("query", query)
                .str("filter", filter)
                .str("in", scope)
                .paging(paging)
                .str("order", order),
        ),
        Mail::Thread {
            message_id,
            download_attachments,
        } => (
            "thread",
            p.str("message_id", Some(message_id))
                .flag("download_attachments", download_attachments),
        ),
        Mail::Attachment { id, stream: s } => {
            stream = s;
            ("attachment", p.str("id", Some(id)))
        }
        Mail::Draft {
            mode,
            to,
            cc,
            bcc,
            subject,
            body,
            account,
            edit,
            reply_to,
            reply_all,
            forward,
            delete,
            attach,
            attach_id,
            attach_stream,
            template,
            placeholder,
            no_signature,
        } => {
            if mode.as_deref().is_some_and(|m| m != "signatures") {
                bail!(
                    "The only argument draft takes is `signatures`; everything else is a flag (see spark-cli draft --help)."
                );
            }
            let mut files: Vec<String> = attach
                .iter()
                .map(|p| absolute(p).display().to_string())
                .collect();
            if let Some(name) = attach_stream {
                let piped = read_stdin_file(&name)?;
                files.push(piped.path.display().to_string());
                upload = Some(piped);
            }
            (
                "draft",
                p.str("mode", mode)
                    .list("to", to)
                    .list("cc", cc)
                    .list("bcc", bcc)
                    .str("subject", subject)
                    .str("body", body)
                    .str("account", account)
                    .str("edit", edit)
                    .str("reply_to", reply_to)
                    .str("reply_all", reply_all)
                    .str("forward", forward)
                    .str("delete", delete)
                    .list("attach", files)
                    .list("attach_id", attach_id)
                    .str("template", template)
                    .list("placeholder", placeholder)
                    .flag("no_signature", no_signature),
            )
        }
        Mail::Templates {
            personal,
            team,
            paging,
        } => (
            "templates",
            p.flag("personal", personal)
                .str("team", team)
                .paging(paging),
        ),
        Mail::Template { reference } => ("template", p.str("ref", Some(reference))),
        Mail::Events { range, scope } => ("events", p.range(range).str("in", scope)),
        Mail::Event {
            mode,
            event_id,
            status,
            title,
            start,
            end,
            all_day,
            description,
            alerts,
            location,
            video_conference,
            show_as,
            calendar,
            add,
            remove,
        } => (
            "event",
            p.str("mode", Some(mode))
                .str("event_id", event_id)
                .str("status", status)
                .str("title", title)
                .str("start", start)
                .str("end", end)
                .flag("all_day", all_day)
                .str("description", description)
                .str("alerts", alerts)
                .str("location", location)
                .str("video_conference", video_conference)
                .str("show_as", show_as)
                .str("calendar", calendar)
                .list("add", add)
                .list("remove", remove),
        ),
        Mail::Availability { range, attendees } => {
            ("availability", p.range(range).str("attendees", attendees))
        }
        Mail::Contacts { query, limit } => ("contacts", p.str("query", query).int("limit", limit)),
        Mail::Contact { email } => ("contact", p.str("email", Some(email))),
        Mail::Action {
            action_name,
            message_ids,
            date,
            folder,
            team,
            user,
            assignee,
            comment,
        } => (
            "action",
            p.str("action_name", Some(action_name))
                .list("message_ids", message_ids)
                .str("date", date)
                .str("folder", folder)
                .str("team", team)
                .list("user", user)
                .str("assignee", assignee)
                .str("comment", comment),
        ),
        Mail::ContactAction {
            action_name,
            emails,
        } => (
            "contact-action",
            p.str("action_name", Some(action_name))
                .list("emails", emails),
        ),
        Mail::History {
            undo,
            paging,
            verbose,
            access,
            result,
            from,
            to,
        } => (
            "history",
            p.paging(paging)
                .flag("verbose", verbose)
                .str("access", access)
                .str("result", result)
                .str("from", from)
                .str("to", to)
                .str("undo", undo.map(|Undo::Undo { id }| id.to_string())),
        ),
    };
    let result = Link::new(&caller())
        .request("tools/call", json!({ "name": tool, "arguments": params.0 }))
        .await;
    // spark-rs has read the piped file by now.
    drop(upload);
    let result = result?;
    let text = result["content"][0]["text"].as_str().unwrap_or_default();
    if result["isError"].as_bool().unwrap_or(false) {
        bail!("{text}");
    }
    if stream {
        let path = result["structuredContent"]["path"]
            .as_str()
            .context("spark-rs didn't say where the file is. Update spark-rs.")?;
        let mut file = std::fs::File::open(path).with_context(|| format!("opening {path}"))?;
        let mut out = std::io::stdout().lock();
        std::io::copy(&mut file, &mut out)?;
        out.flush()?;
        return Ok(());
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&result["structuredContent"])?
        );
    } else {
        println!("{text}");
    }
    Ok(())
}

/// Who runs spark-cli, for spark-rs's activity log: `AI_AGENT` when set,
/// as Spark's CLI reads it, else an agent known by what it sets.
fn caller() -> String {
    if let Ok(name) = std::env::var("AI_AGENT")
        && !name.trim().is_empty()
    {
        return name.trim().to_owned();
    }
    let set = |var: &str| std::env::var_os(var).is_some();
    if set("CLAUDECODE") {
        "claude-code".into()
    } else if set("GEMINI_CLI") {
        "gemini-cli".into()
    } else if set("CODEX_SANDBOX") || set("CODEX_THREAD_ID") {
        "codex".into()
    } else {
        "spark-cli".into()
    }
}

/// A path as spark-rs, which runs elsewhere, can find it.
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_owned())
}

/// A file piped in for `--attach-stream`, in a folder of its own that goes
/// when this does: once spark-rs has read it, or as soon as saving fails.
struct Upload {
    dir: PathBuf,
    path: PathBuf,
}

impl Drop for Upload {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Saves what's piped to stdin as `name`, for spark-rs to attach.
fn read_stdin_file(name: &str) -> Result<Upload> {
    let name = Path::new(name)
        .file_name()
        .context("--attach-stream needs a file name, e.g. --attach-stream report.pdf")?;
    let mut bytes = Vec::new();
    std::io::stdin()
        .read_to_end(&mut bytes)
        .context("reading stdin")?;
    if bytes.is_empty() {
        bail!(
            "--attach-stream reads the file from stdin; pipe it in, e.g. `cat report.pdf | spark-cli draft --attach-stream report.pdf …`."
        );
    }
    let dir = std::env::temp_dir()
        .join("spark-rs")
        .join("agent-uploads")
        .join(format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
    std::fs::create_dir_all(&dir)?;
    let upload = Upload {
        path: dir.join(name),
        dir,
    };
    std::fs::write(&upload.path, bytes)?;
    Ok(upload)
}

/// `spark-cli skill`: prints a skill (the base one unless named), lists
/// them, or installs one or all of them as `<dir>/<name>/SKILL.md`.
pub fn skill(name: Option<String>, install: Option<PathBuf>, all: bool, list: bool) -> Result<()> {
    if list {
        for (n, text) in SKILLS {
            println!("{n}\n    {}\n", description(text));
        }
        println!(
            "spark-cli skill <name> prints one; spark-cli skill --install <folder> --all installs them all."
        );
        return Ok(());
    }
    let wanted: Vec<&str> = match (&name, all) {
        (Some(_), true) => bail!("Pass a skill's name or --all, not both."),
        (_, true) => SKILLS.iter().map(|(n, _)| *n).collect(),
        (Some(n), false) => vec![n.as_str()],
        (None, false) => vec![BASE],
    };
    let mut skills = Vec::new();
    for n in wanted {
        let found = skill_text(n)
            .with_context(|| format!("There's no skill {n}. spark-cli skill --list lists them."))?;
        skills.push(found);
    }
    let Some(dir) = install else {
        for (_, text) in &skills {
            print!("{text}");
        }
        return Ok(());
    };
    for (n, text) in &skills {
        let folder = dir.join(n);
        std::fs::create_dir_all(&folder).with_context(|| format!("making {}", folder.display()))?;
        let path = folder.join("SKILL.md");
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        println!("Installed {}", path.display());
    }
    if !skills.iter().any(|(n, _)| *n == BASE) && !dir.join(BASE).join("SKILL.md").is_file() {
        println!(
            "The others build on {BASE}: spark-cli skill --install {} installs it.",
            dir.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_base_skill_names_its_version_and_commands() {
        let (_, skill) = skill_text(BASE).unwrap();
        assert!(skill.starts_with("---\nname: use-spark-rs\n"));
        assert!(skill.contains(&format!("version: {}", env!("CARGO_PKG_VERSION"))));
        for command in [
            "accounts",
            "emails",
            "thread",
            "draft",
            "action send",
            "history undo",
        ] {
            assert!(skill.contains(&format!("spark-cli {command}")), "{command}");
        }
    }

    #[test]
    fn every_skill_is_named_for_its_folder_and_builds_on_the_base() {
        assert!(SKILLS.len() > 1);
        for (name, text) in SKILLS {
            assert!(
                text.starts_with(&format!("---\nname: {name}\n")),
                "{name}: name"
            );
            assert!(!description(text).is_empty(), "{name}: description");
            assert!(text.contains("version: {{version}}"), "{name}: version");
            if *name != BASE {
                assert!(name.starts_with(PREFIX), "{name}: prefix");
                assert!(text.contains("      - use-spark-rs\n"), "{name}: requires");
            }
        }
        assert_eq!(
            skill_text("inbox-triage").map(|(n, _)| n),
            Some("spark-rs-inbox-triage")
        );
    }

    /// Splits a shell command line into words: quotes, `$'…'` with its
    /// escapes, and backslashes; stops at a pipe, redirect or comment.
    fn words(line: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut word: Option<String> = None;
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '|' | '>' | ';' | '&' => break,
                '#' if word.is_none() => break,
                c if c.is_whitespace() => out.extend(word.take()),
                '\'' => {
                    let w = word.get_or_insert_with(String::new);
                    for c in chars.by_ref() {
                        if c == '\'' {
                            break;
                        }
                        w.push(c);
                    }
                }
                '"' => {
                    let w = word.get_or_insert_with(String::new);
                    while let Some(c) = chars.next() {
                        match c {
                            '"' => break,
                            '\\' => w.extend(chars.next()),
                            c => w.push(c),
                        }
                    }
                }
                '$' if chars.peek() == Some(&'\'') => {
                    chars.next();
                    let w = word.get_or_insert_with(String::new);
                    while let Some(c) = chars.next() {
                        match c {
                            '\'' => break,
                            '\\' => match chars.next() {
                                Some('n') => w.push('\n'),
                                Some('t') => w.push('\t'),
                                Some(other) => w.push(other),
                                None => {}
                            },
                            c => w.push(c),
                        }
                    }
                }
                '\\' => word.get_or_insert_with(String::new).extend(chars.next()),
                c => word.get_or_insert_with(String::new).push(c),
            }
        }
        out.extend(word);
        out
    }

    #[test]
    fn shell_words_split_like_bash() {
        assert_eq!(
            words(r#"spark-cli draft --body $'Hi,\n\nThanks.' --to "Ana <a@x.com>" | jq . # note"#),
            [
                "spark-cli",
                "draft",
                "--body",
                "Hi,\n\nThanks.",
                "--to",
                "Ana <a@x.com>"
            ]
        );
        assert_eq!(
            words("spark-cli emails 'Set Aside' --json"),
            ["spark-cli", "emails", "Set Aside", "--json"]
        );
    }

    /// Every `spark-cli` line in the skills' bash examples parses as a
    /// command, so a skill can't teach a flag that doesn't exist.
    #[test]
    fn every_example_in_the_skills_is_a_real_command() {
        let mut checked = 0;
        let mut bad = Vec::new();
        for (name, text) in SKILLS {
            let mut in_bash = false;
            for (n, line) in text.lines().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("```") {
                    in_bash = trimmed == "```bash";
                    continue;
                }
                let Some(at) = line.find("spark-cli ").filter(|_| in_bash) else {
                    continue;
                };
                let args = words(&line[at..]);
                checked += 1;
                if let Err(e) = <crate::Cli as clap::Parser>::try_parse_from(&args) {
                    let first = e.to_string().lines().next().unwrap_or_default().to_owned();
                    bad.push(format!("{name}:{}: {}\n    {first}", n + 1, line.trim()));
                }
            }
        }
        assert!(checked > 50, "only {checked} examples found");
        assert!(
            bad.is_empty(),
            "examples that don't parse:\n{}",
            bad.join("\n")
        );
    }

    #[test]
    fn params_leave_out_what_wasnt_given() {
        let p = Params::default()
            .str("a", None)
            .str("b", Some("x".into()))
            .list("c", vec![])
            .flag("d", false)
            .flag("e", true);
        assert_eq!(Value::Object(p.0), json!({ "b": "x", "e": true }));
    }
}
