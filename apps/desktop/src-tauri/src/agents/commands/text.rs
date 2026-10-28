//! Commands' results as text to read, the default output; `--json` gives
//! the values themselves. Lists are tables, an email is headers then text.

use serde_json::Value;

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or_default()
}

/// `2026-10-27T09:12:00+01:00` as `2026-10-27 09:12`.
fn short_time(iso: &str) -> String {
    match (iso.get(..10), iso.get(11..16)) {
        (Some(day), Some(time)) => format!("{day} {time}"),
        _ => iso.to_owned(),
    }
}

/// Cuts to `max` characters with an ellipsis.
fn clip(text: &str, max: usize) -> String {
    let text = text.replace(['\n', '\r', '\t'], " ");
    if text.chars().count() <= max {
        return text;
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Each line indented by two spaces.
pub fn indent(text: &str) -> String {
    text.lines()
        .map(|l| {
            if l.is_empty() {
                String::new()
            } else {
                format!("  {l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn size(bytes: u64) -> String {
    match bytes {
        0 => "unknown".into(),
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{:.1} MB", b as f64 / 1024.0 / 1024.0),
    }
}

/// Columns padded to their widest cell.
pub fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let line = |cells: Vec<&str>| {
        let last = cells.len() - 1;
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                if i == last {
                    (*c).to_owned()
                } else {
                    format!("{c:<width$}", width = widths[i])
                }
            })
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_owned()
    };
    let mut out = vec![line(headers.to_vec())];
    out.extend(
        rows.iter()
            .map(|r| line(r.iter().map(String::as_str).collect())),
    );
    out.join("\n")
}

/// Any value as `Key: value` lines, for results without a layout of their own.
pub fn generic(value: &Value) -> String {
    let mut out = String::new();
    write(&mut out, value, 0);
    out.trim_end().to_owned()
}

fn label(key: &str) -> String {
    let words = key.replace('_', " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn write(out: &mut String, value: &Value, depth: usize) {
    let pad = "  ".repeat(depth);
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                match v {
                    Value::Null => {}
                    Value::Array(items) if items.is_empty() => {}
                    Value::Object(_) | Value::Array(_) => {
                        out.push_str(&format!("{pad}{}:\n", label(k)));
                        write(out, v, depth + 1);
                    }
                    other => out.push_str(&format!("{pad}{}: {}\n", label(k), scalar(other))),
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                match item {
                    Value::Object(_) => {
                        write(out, item, depth);
                        out.push('\n');
                    }
                    other => out.push_str(&format!("{pad}- {}\n", scalar(other))),
                }
            }
        }
        other => out.push_str(&format!("{pad}{}\n", scalar(other))),
    }
}

fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "yes".into(),
        Value::Bool(false) => "no".into(),
        other => other.to_string(),
    }
}

pub fn accounts(value: &Value) -> String {
    let mut out = String::new();
    for a in value["accounts"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "Email Account: {} ({}) - {}, \"{}\"\n",
            s(&a["email"]),
            s(&a["access"]),
            s(&a["provider"]),
            s(&a["label"])
        ));
        if !s(&a["name"]).is_empty() {
            out.push_str(&format!("  Name: {}\n", s(&a["name"])));
        }
        let calendars: Vec<String> = a["calendars"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| {
                let mut name = format!("{} ({})", s(&c["name"]), s(&c["identifier"]));
                if c["is_default"].as_bool() == Some(true) {
                    name.push_str(" default");
                }
                if c["own"].as_bool() == Some(false) {
                    name.push_str(" shared with you");
                }
                name
            })
            .collect();
        if !calendars.is_empty() {
            out.push_str(&format!("  Calendars: {}\n", calendars.join(", ")));
        }
        match a["signature"].as_str() {
            Some(sig) if !sig.trim().is_empty() => {
                out.push_str(&format!("  Signature:\n{}\n", indent(&indent(sig.trim()))));
            }
            _ => out.push_str("  Signature: none\n"),
        }
        out.push('\n');
    }
    out.push_str("Access: read lists and reads mail; triage also changes it and writes drafts; send also sends and changes calendar events.");
    out
}

pub fn folders(value: &Value) -> String {
    let mut out = String::new();
    for a in value["accounts"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "Email Account: {}{}\n",
            s(&a["account"]),
            if a["gmail_labels"].as_bool() == Some(true) {
                " (Gmail labels)"
            } else {
                ""
            }
        ));
        let rows: Vec<Vec<String>> = a["folders"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|f| {
                let unread = f["unread"].as_u64().unwrap_or(0);
                vec![
                    format!("  {}", s(&f["name"])),
                    format!("({})", s(&f["identifier"])),
                    if unread > 0 {
                        format!("{}, {unread} unread", f["total"])
                    } else {
                        f["total"].to_string()
                    },
                ]
            })
            .collect();
        out.push_str(&table(&["  Folder", "Identifier", "Emails"], &rows));
        out.push_str("\n\n");
    }
    out.push_str("A bare name (Inbox, Done, Sent, Projects) means that folder in every account.");
    out
}

fn flags(e: &Value) -> String {
    let mut f = Vec::new();
    if e["unread"].as_bool() == Some(true) {
        f.push("unread".to_owned());
    }
    if e["pinned"].as_bool() == Some(true) {
        f.push("pinned".into());
    }
    if e["priority"].as_bool() == Some(true) {
        f.push("priority".into());
    }
    if e["has_attachment"].as_bool() == Some(true) {
        f.push("attachment".into());
    }
    if e["new_sender"].as_bool() == Some(true) {
        f.push("new sender".into());
    }
    if e["muted"].as_bool() == Some(true) {
        f.push("muted".into());
    }
    if let Some(until) = e["snoozed_until"].as_str() {
        f.push(format!("snoozed until {}", short_time(until)));
    }
    if let Some(kind) = e["invite"].as_str() {
        f.push(kind.replace('_', " "));
    }
    if let Some(n) = e["messages"].as_u64() {
        f.push(format!("{n} emails"));
    }
    f.join(", ")
}

fn from(e: &Value) -> String {
    let name = s(&e["from_name"]);
    if name.is_empty() {
        s(&e["from_address"]).to_owned()
    } else {
        format!("{name} <{}>", s(&e["from_address"]))
    }
}

pub fn emails(value: &Value) -> String {
    let list = value["emails"].as_array().cloned().unwrap_or_default();
    let total = value["total"].as_u64().unwrap_or(0);
    let page = value["page"].as_u64().unwrap_or(1);
    let size = value["page_size"].as_u64().unwrap_or(50);
    let first = (page - 1) * size;
    let mut out = s(&value["folder"]).to_owned();
    if let Some(f) = value["filter"].as_str() {
        out.push_str(&format!(", {f}"));
    }
    if list.is_empty() {
        out.push_str(&format!(
            ": nothing here{}.",
            if page > 1 { " on this page" } else { "" }
        ));
    } else {
        out.push_str(&format!(
            ": {}–{} of {total} conversations, newest email of each.",
            first + 1,
            first + list.len() as u64
        ));
    }
    if let Some(n) = value["new_senders"].as_u64().filter(|&n| n > 0) {
        out.push_str(&format!(
            " New Senders: {n} waiting (emails --new-senders)."
        ));
    }
    out.push('\n');
    let rows: Vec<Vec<String>> = list
        .iter()
        .map(|e| {
            vec![
                e["id"].to_string().trim_matches('"').to_owned(),
                short_time(s(&e["date"])),
                clip(&from(e), 32),
                clip(s(&e["subject"]), 60),
                flags(e),
            ]
        })
        .collect();
    if !rows.is_empty() {
        out.push_str(&table(&["ID", "Date", "From", "Subject", "Flags"], &rows));
    }
    if first + (list.len() as u64) < total {
        out.push_str(&format!("\nMore: page {}.", page + 1));
    }
    out
}

/// A topic search's results, each with its text.
pub fn found(value: &Value) -> String {
    let list = value["emails"].as_array().cloned().unwrap_or_default();
    let mut out = format!(
        "{} for \"{}\" in {}.\n",
        match list.len() {
            0 => "Nothing found".to_owned(),
            n => format!("{n} best of {} found", value["total"]),
        },
        s(&value["query"]),
        s(&value["folder"])
    );
    for e in &list {
        out.push_str(&format!(
            "\nID: {}\nDate: {}\nFrom: {}\nTo: {}\nSubject: {}\nFolder: {}{}\n",
            e["id"],
            short_time(s(&e["date"])),
            from(e),
            join(&e["to"]),
            s(&e["subject"]),
            s(&e["folder"]),
            match flags(e) {
                f if f.is_empty() => String::new(),
                f => format!("\nFlags: {f}"),
            }
        ));
        out.push_str(&indent(s(&e["text"])));
        if e["body_missing"].as_bool() == Some(true) {
            out.push_str("\n  [Only the start: thread downloads the rest.]");
        }
        out.push('\n');
    }
    out.trim_end().to_owned()
}

fn join(v: &Value) -> String {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn thread(value: &Value) -> String {
    let messages = value["messages"].as_array().cloned().unwrap_or_default();
    let mut out = format!(
        "Thread: {} ({}, {})\n",
        s(&value["subject"]),
        match messages.len() {
            1 => "1 email".to_owned(),
            n => format!("{n} emails"),
        },
        s(&value["account"])
    );
    let labels = join(&value["labels"]);
    if !labels.is_empty() {
        out.push_str(&format!("Labels: {labels}\n"));
    }
    for m in &messages {
        out.push_str(&format!("\n--- ID: {} ---\nFrom: {}\n", m["id"], from(m)));
        if m["reply_to"].is_array() {
            out.push_str(&format!("Reply-To: {}\n", join(&m["reply_to"])));
        }
        out.push_str(&format!("To: {}\n", join(&m["to"])));
        if !join(&m["cc"]).is_empty() {
            out.push_str(&format!("Cc: {}\n", join(&m["cc"])));
        }
        if !join(&m["bcc"]).is_empty() {
            out.push_str(&format!("Bcc: {}\n", join(&m["bcc"])));
        }
        out.push_str(&format!(
            "Date: {}\nSubject: {}\nFolder: {}\n",
            short_time(s(&m["date"])),
            s(&m["subject"]),
            s(&m["folder"])
        ));
        let f = flags(m);
        if !f.is_empty() {
            out.push_str(&format!("Flags: {f}\n"));
        }
        if let Some(i) = m["invitation"].as_object() {
            let when = match (
                i.get("start").and_then(Value::as_str),
                i.get("end").and_then(Value::as_str),
            ) {
                (Some(start), Some(end)) => format!("{} – {}", short_time(start), short_time(end)),
                (Some(start), None) => short_time(start),
                _ => String::new(),
            };
            let kind = i
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("invitation");
            out.push_str(&format!(
                "Calendar {kind}: {}, {when}{}{}\n",
                i.get("title").and_then(Value::as_str).unwrap_or_default(),
                i.get("location")
                    .and_then(Value::as_str)
                    .map(|l| format!(", {l}"))
                    .unwrap_or_default(),
                if i.get("canceled") == Some(&Value::Bool(true)) {
                    ", canceled"
                } else {
                    ""
                },
            ));
            match i.get("answer").and_then(Value::as_str) {
                Some(a) => out.push_str(&format!("Your answer: {a}\n")),
                None if i.get("can_answer") == Some(&Value::Bool(true)) => out.push_str(
                    "Your answer: none yet (event rsvp <this email's ID> accept|decline|maybe)\n",
                ),
                None => {}
            }
        }
        let files = m["attachments"].as_array().cloned().unwrap_or_default();
        if !files.is_empty() {
            out.push_str("Attachments:\n");
            let rows: Vec<Vec<String>> = files
                .iter()
                .map(|a| {
                    vec![
                        format!("  {}", s(&a["id"])),
                        s(&a["name"]).to_owned(),
                        size(a["size"].as_u64().unwrap_or(0)),
                        s(&a["mime_type"]).to_owned(),
                        a["path"].as_str().map_or(
                            "(not downloaded; attachment <ID> downloads it)".to_owned(),
                            str::to_owned,
                        ),
                    ]
                })
                .collect();
            out.push_str(&table(
                &["  ID", "Name", "Size", "MIME Type", "Path"],
                &rows,
            ));
            out.push('\n');
        }
        out.push('\n');
        out.push_str(s(&m["text"]).trim_end());
        if m["body_missing"].as_bool() == Some(true) {
            out.push_str("\n[Only the start: the rest couldn't be downloaded.]");
        }
        out.push('\n');
    }
    for p in value["problems"].as_array().into_iter().flatten() {
        out.push_str(&format!("\nNote: {}", s(p)));
    }
    out.trim_end().to_owned()
}

pub fn events(value: &Value) -> String {
    let list = value["events"].as_array().cloned().unwrap_or_default();
    let mut out = format!(
        "Events from {} to {}: {}\n",
        short_time(s(&value["start"])),
        short_time(s(&value["end"])),
        if list.is_empty() {
            "none".to_owned()
        } else {
            list.len().to_string()
        }
    );
    for e in &list {
        let time = if e["all_day"].as_bool() == Some(true) {
            format!("{} all day", &short_time(s(&e["start"]))[..10])
        } else {
            format!(
                "{} – {}",
                short_time(s(&e["start"])),
                short_time(s(&e["end"])).get(11..).unwrap_or_default()
            )
        };
        out.push_str(&format!(
            "\n{}  {}  {}\n",
            s(&e["id"]),
            time,
            s(&e["title"])
        ));
        out.push_str(&format!(
            "  Calendar: {}:{}\n",
            s(&e["account"]),
            s(&e["calendar"])
        ));
        for (key, name) in [
            ("location", "Location"),
            ("call_link", "Join"),
            ("organizer", "Organizer"),
        ] {
            if let Some(v) = e[key].as_str() {
                out.push_str(&format!("  {name}: {v}\n"));
            }
        }
        let people = join(&e["attendees"]);
        if !people.is_empty() {
            out.push_str(&format!("  Attendees: {people}\n"));
        }
        let response = s(&e["response"]);
        if !response.is_empty() && response != "none" {
            out.push_str(&format!("  You: {response}\n"));
        }
        if e["canceled"].as_bool() == Some(true) {
            out.push_str("  Canceled\n");
        }
        if let Some(d) = e["description"].as_str() {
            out.push_str(&format!("  Description: {}\n", clip(d, 300)));
        }
    }
    for p in value["problems"].as_array().into_iter().flatten() {
        out.push_str(&format!("\nNote: {}", s(p)));
    }
    out.trim_end().to_owned()
}

pub fn templates(value: &Value) -> String {
    let list = value["templates"].as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        return "No templates yet. The user makes them in spark-rs's composer.".into();
    }
    let rows: Vec<Vec<String>> = list
        .iter()
        .map(|t| {
            vec![
                t["id"].to_string(),
                s(&t["scope"]).to_owned(),
                s(&t["name"]).to_owned(),
                clip(s(&t["subject"]), 40),
                t["last_used"].as_str().map(short_time).unwrap_or_default(),
            ]
        })
        .collect();
    table(&["ID", "Scope", "Name", "Subject", "Last used"], &rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tables_pad_all_but_the_last_column() {
        let t = table(
            &["ID", "Name"],
            &[
                vec!["1".into(), "Ana".into()],
                vec!["100".into(), "Bo".into()],
            ],
        );
        assert_eq!(t, "ID   Name\n1    Ana\n100  Bo");
    }

    #[test]
    fn generic_text_lists_keys_and_skips_empties() {
        let text =
            generic(&json!({ "email": "a@b.c", "is_priority": true, "none": null, "tags": [] }));
        assert_eq!(text, "Email: a@b.c\nIs priority: yes");
    }

    #[test]
    fn clips_long_lines() {
        assert_eq!(clip("abcdef", 4), "abc…");
        assert_eq!(clip("a\nb", 10), "a b");
    }
}
