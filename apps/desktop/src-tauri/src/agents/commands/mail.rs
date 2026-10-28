//! Reading mail: `accounts`, `folders`, `emails`, `search`, `thread` and
//! `attachment`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use serde_json::{Value, json};
use spark_store::{Account, AccountKind, Category, Message, MessageState, now_ms};

use super::filter::{Filter, List, Place, Scope, View};
use super::{Args, Done, Env, Fail, MAX_TEXT, Reply, count, cut, db, iso, message_id, text};
use crate::agents::Access;
use crate::files;

/// How many emails a page lists unless asked.
const PAGE: usize = 50;
/// How many emails a topic search returns, with their text.
const TOPIC_RESULTS: usize = 20;
/// Each email's text in a topic search is cut here.
const TOPIC_TEXT: usize = 8_000;

pub fn accounts(env: &Env) -> Done {
    let shared = env.readable()?;
    let store = env.store();
    let calendars = store.calendars().map_err(db)?;
    let mut list = Vec::new();
    for (a, access) in &shared {
        let signature = store.signature_for(&a.email).map_err(db)?;
        let calendars: Vec<Value> = calendars
            .iter()
            .filter(|c| c.account_id == a.id)
            .map(|c| {
                json!({
                    "id": c.id,
                    "name": c.name,
                    "identifier": format!("{}:{}", a.email, c.name),
                    "is_default": c.is_default,
                    "own": c.own,
                })
            })
            .collect();
        list.push(json!({
            "id": a.id,
            "email": a.email,
            "name": a.name,
            "label": a.label,
            "provider": provider(a.kind),
            "access": access.name(),
            "calendars": calendars,
            "signature": signature.map(|s| s.text),
        }));
    }
    let value = json!({ "accounts": list });
    Ok(Reply::new(value.clone(), text::accounts(&value)).said("viewed your accounts"))
}

pub fn provider(kind: AccountKind) -> &'static str {
    match kind {
        AccountKind::Gmail => "Gmail",
        AccountKind::Microsoft => "Microsoft 365",
        AccountKind::Imap => "IMAP",
    }
}

/// Reads the shared accounts' mail, or only `only`'s.
fn view(env: &Env, only: Option<&str>) -> Result<View, Fail> {
    let mut accounts: Vec<Account> = env.readable()?.into_iter().map(|(a, _)| a).collect();
    if let Some(email) = only {
        accounts.retain(|a| a.email.eq_ignore_ascii_case(email.trim()));
        if accounts.is_empty() {
            return Err(Fail::Error(format!(
                "There's no account {email} shared with agents. `accounts` lists them."
            )));
        }
    }
    View::read(&env.store(), accounts)
}

pub fn folders(env: &Env, a: &Args) -> Done {
    let view = view(env, a.str("account"))?;
    let mut out = Vec::new();
    for account in &view.accounts {
        let mine: Vec<&Message> = view
            .messages
            .iter()
            .filter(|m| m.account_id == account.id)
            .collect();
        let tally = |hit: &dyn Fn(&Message) -> bool| {
            let found: Vec<&&Message> = mine.iter().filter(|m| hit(m)).collect();
            (found.len(), found.iter().filter(|m| m.unread).count())
        };
        let mut folders = Vec::new();
        for list in List::ALL {
            if list == List::NewSenders && !view.screen_before {
                continue;
            }
            let (total, unread) = tally(&|m| view.is_in(m, list));
            folders.push(json!({
                "name": list.name(),
                "identifier": format!("{}:{}", account.email, list.name()),
                "kind": "system",
                "total": total,
                "unread": unread,
            }));
        }
        let mut own: Vec<_> = view
            .folders
            .iter()
            .filter(|f| f.account_id == account.id && f.role.is_none() && f.selectable)
            .collect();
        own.sort_by_key(|f| view.path(f.id).to_lowercase());
        for f in own {
            let (total, unread) = tally(&|m| m.folder == Some(f.id) || m.folders.contains(&f.id));
            folders.push(json!({
                "name": view.path(f.id),
                "identifier": view.qualified(f),
                "kind": if account.kind == AccountKind::Gmail { "label" } else { "folder" },
                "total": total,
                "unread": unread,
            }));
        }
        out.push(json!({
            "account": account.email,
            "provider": provider(account.kind),
            "gmail_labels": account.kind == AccountKind::Gmail,
            "folders": folders,
        }));
    }
    let value = json!({ "accounts": out });
    Ok(Reply::new(value.clone(), text::folders(&value)).said("viewed your folders"))
}

pub fn emails(env: &Env, a: &Args) -> Done {
    let mut args = a.list("folder");
    args.retain(|f| !f.is_empty());
    if let [only] = args.as_slice() {
        let (account, list) = match only.split_once(':') {
            Some((who, list)) if who.contains('@') => (Some(who.trim()), list.trim()),
            _ => (None, only.trim()),
        };
        if list.eq_ignore_ascii_case("scheduled") {
            return scheduled(env, account);
        }
    }
    let view = view(env, None)?;
    let new_senders = a.flag("new_senders");
    let default = if new_senders {
        List::NewSenders
    } else {
        List::Inbox
    };
    let scopes: Vec<Scope> = if args.is_empty() {
        vec![view.scope(None, Place::List(default))?]
    } else {
        args.iter()
            .map(|f| {
                let mut scope = view.scope(Some(f), Place::List(default))?;
                if new_senders && scope.place == Place::List(List::Inbox) {
                    scope.place = Place::List(List::NewSenders);
                }
                Ok(scope)
            })
            .collect::<Result<_, Fail>>()?
    };
    let label = scopes
        .iter()
        .map(|s| s.label.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let mut listed = list(env, &view, &scopes, a)?;
    // As Spark's Inbox says how many new senders wait above it.
    let screened = scopes.iter().any(|s| s.place == Place::List(List::Inbox)) && view.screen_before;
    if screened {
        let waiting: HashSet<i64> = view
            .messages
            .iter()
            .filter(|m| {
                view.is_in(m, List::NewSenders)
                    && scopes.iter().any(|s| s.accounts.contains(&m.account_id))
            })
            .map(|m| m.conversation)
            .collect();
        listed["new_senders"] = json!(waiting.len());
    }
    listed["folder"] = json!(label);
    let summary = if new_senders {
        "reviewed email from new senders".to_owned()
    } else {
        match a.str("filter") {
            Some(f) => format!("browsed {label} for {f}"),
            None => format!("browsed {label}"),
        }
    };
    Ok(Reply::new(listed.clone(), text::emails(&listed)).said(summary))
}

/// Mail waiting to go (`outbox`). IDs are `s` and the outbox's.
fn scheduled(env: &Env, only: Option<&str>) -> Done {
    let shared: Vec<Account> = env.readable()?.into_iter().map(|(a, _)| a).collect();
    if let Some(email) = only
        && !shared.iter().any(|a| a.email.eq_ignore_ascii_case(email))
    {
        return Err(Fail::Error(format!(
            "There's no account {email} shared with agents."
        )));
    }
    let waiting = super::outbox(env)?;
    let mut emails = Vec::new();
    let mut rows = Vec::new();
    for w in &waiting {
        let Some(account) = shared.iter().find(|a| a.id == w.account_id) else {
            continue;
        };
        if only.is_some_and(|e| !account.email.eq_ignore_ascii_case(e)) {
            continue;
        }
        let mut row = json!({
            "id": format!("s{}", w.id),
            "account": account.email,
            "folder": "Scheduled",
            "date": iso(w.send_at),
            "from_name": account.name,
            "from_address": account.email,
            "to": w.draft.to,
            "subject": w.draft.subject,
            "snippet": spark_mail::parse::snippet(&w.draft.body),
            "sending": w.sending,
        });
        if let Some(why) = &w.failed {
            row["failed"] = json!(why);
        }
        if let Some(draft) = w.draft.draft_id {
            row["draft"] = json!(draft);
        }
        rows.push(vec![
            format!("s{}", w.id),
            super::when(w.send_at),
            w.draft.to.clone(),
            w.draft.subject.clone(),
            match (w.sending, &w.failed) {
                (true, _) => "sending".to_owned(),
                (_, Some(why)) => format!("failed, tried again: {why}"),
                _ => String::new(),
            },
        ]);
        emails.push(row);
    }
    let mut out = format!(
        "Scheduled: {}\n",
        if emails.is_empty() {
            "nothing waiting to send".to_owned()
        } else {
            count(emails.len(), "email", "emails")
        }
    );
    if !rows.is_empty() {
        out.push_str(&text::table(
            &["ID", "Sends at", "To", "Subject", "Status"],
            &rows,
        ));
        out.push_str("\naction send <ID> sends now (date= moves it); action unschedule <ID> takes it back to Drafts.");
    }
    let value = json!({ "folder": "Scheduled", "total": emails.len(), "emails": emails });
    Ok(Reply::new(value, out.trim_end().to_owned()).said("browsed Scheduled"))
}

pub fn search(env: &Env, a: &Args) -> Done {
    let view = view(env, None)?;
    let scope = view.scope(a.str("in"), Place::Everywhere)?;
    let Some(query) = a.str("query").or(a.str("about")) else {
        let mut listed = list(env, &view, std::slice::from_ref(&scope), a)?;
        listed["folder"] = json!(scope.label);
        let summary = match a.str("filter") {
            Some(f) => format!("searched your emails for {f}"),
            None => "searched your emails".to_owned(),
        };
        return Ok(Reply::new(listed.clone(), text::emails(&listed)).said(summary));
    };
    let filter = Filter::parse(a.str("filter").unwrap_or(""))?;
    let store = env.store();
    let ranked = store.search_ranked(query).map_err(db)?;
    let by_id: HashMap<i64, &Message> = view.messages.iter().map(|m| (m.id, m)).collect();
    let candidates: Vec<&Message> = ranked
        .iter()
        .filter_map(|id| by_id.get(id).copied())
        .filter(|m| view.in_scope(m, &scope))
        .collect();
    let found = match filter.words() {
        Some(words) => Some(store.search_ids(words).map_err(db)?.into_iter().collect()),
        None => None,
    };
    let hits = filter.apply(&store, &view, candidates, found.as_ref(), now_ms())?;
    let total = hits.len();
    let mut emails = Vec::new();
    for m in hits.into_iter().take(TOPIC_RESULTS) {
        let mut row = summary(&view, m);
        if let Some(content) = store.content(m.id).map_err(db)? {
            row["to"] = json!(content.to);
            row["cc"] = json!(content.cc);
            row["text"] = json!(cut(content.text, TOPIC_TEXT));
            if content.body_missing {
                row["body_missing"] = json!(true);
            }
        }
        emails.push(row);
    }
    let value = json!({
        "query": query,
        "folder": scope.label,
        "total": total,
        "emails": emails,
        "note": "Keyword search, best matches first. thread <id> reads a whole conversation.",
    });
    Ok(Reply::new(value.clone(), text::found(&value))
        .said(format!("searched your emails for “{query}”")))
}

/// A page of conversations in `scopes`, newest first unless asked: each the
/// newest email of it there that the filter matches.
fn list(env: &Env, view: &View, scopes: &[Scope], a: &Args) -> Result<Value, Fail> {
    let filter = Filter::parse(a.str("filter").unwrap_or(""))?;
    let (page, size) = a.paging(PAGE)?;
    let ascending = match a.str("order") {
        None | Some("descending" | "desc") => false,
        Some("ascending" | "asc") => true,
        Some(other) => {
            return Err(Fail::Error(format!(
                "order is ascending or descending, not {other}."
            )));
        }
    };
    let candidates: Vec<&Message> = view
        .messages
        .iter()
        .filter(|m| scopes.iter().any(|s| view.in_scope(m, s)))
        .collect();
    let store = env.store();
    let found: Option<HashSet<i64>> = match filter.words() {
        Some(words) => Some(store.search_ids(words).map_err(db)?.into_iter().collect()),
        None => None,
    };
    let hits = filter.apply(&store, view, candidates, found.as_ref(), now_ms())?;
    drop(store);
    // One row a conversation: its newest match, counting the rest.
    let mut rows: BTreeMap<i64, (&Message, usize)> = BTreeMap::new();
    for m in hits {
        let row = rows.entry(m.conversation).or_insert((m, 0));
        row.1 += 1;
        if (m.date, m.id) > (row.0.date, row.0.id) {
            row.0 = m;
        }
    }
    let mut rows: Vec<(&Message, usize)> = rows.into_values().collect();
    rows.sort_by_key(|(m, _)| (m.date, m.id));
    if !ascending {
        rows.reverse();
    }
    let total = rows.len();
    let emails: Vec<Value> = rows
        .iter()
        .skip((page - 1) * size)
        .take(size)
        .map(|(m, n)| {
            let mut row = summary(view, m);
            if *n > 1 {
                row["messages"] = json!(n);
            }
            row
        })
        .collect();
    Ok(json!({
        "filter": a.str("filter"),
        "page": page,
        "page_size": size,
        "total": total,
        "emails": emails,
    }))
}

/// A list row.
pub fn summary(view: &View, m: &Message) -> Value {
    let mut row = json!({
        "id": m.id,
        "account": view.account(m.account_id).map(|a| a.email.as_str()),
        "folder": view.folder_name(m),
        "date": iso(m.date),
        "from_name": m.from_name,
        "from_address": m.from_address,
        "subject": m.subject,
        "snippet": m.snippet,
        "unread": m.unread,
        "pinned": m.pinned,
        "priority": m.priority,
        "category": category(m.category),
        "has_attachment": m.attachment.is_some(),
        "conversation": m.conversation,
    });
    if let Some(until) = m.snoozed_until.filter(|_| m.state == MessageState::Snoozed) {
        row["snoozed_until"] = json!(iso(until));
    }
    if m.muted {
        row["muted"] = json!(true);
    }
    if m.screening {
        row["new_sender"] = json!(true);
    }
    if let Some(kind) = &m.invite_kind {
        row["invite"] = json!(kind);
    }
    row
}

pub fn category(c: Category) -> &'static str {
    match c {
        Category::People => "personal",
        Category::Notifications => "notification",
        Category::Newsletters => "newsletter",
    }
}

pub async fn thread(env: &Env<'_>, a: &Args<'_>) -> Done {
    let asked = a
        .string("message_id")
        .or_else(|| a.string("id"))
        .ok_or("Pass message_id: an email ID from emails or search.")?;
    let asked = message_id(&asked)?;
    let (m, _) = env.message(asked)?;
    let view = view(env, None)?;
    let elsewhere = |x: &Message| {
        matches!(x.state, MessageState::Deleted | MessageState::Blocked) && x.state != m.state
    };
    let mut members: Vec<&Message> = view
        .messages
        .iter()
        .filter(|x| x.conversation == m.conversation && x.account_id == m.account_id)
        .filter(|x| x.id == m.id || !elsewhere(x))
        .collect();
    members.sort_by_key(|x| (x.date, x.id));
    // Older mail may be stored without its body: download it first.
    let mut problems = Vec::new();
    for x in &members {
        let missing = env.store().body_missing(x.id).map_err(db)?;
        if missing && let Err(e) = env.engine.fetch_body(x.id).await {
            problems.push(format!(
                "Email {}'s text couldn't be downloaded: {e}.",
                x.id
            ));
        }
    }
    let download = a.flag("download_attachments");
    let mut messages = Vec::new();
    let mut labels = Vec::new();
    for x in &members {
        let content = env.store().content(x.id).map_err(db)?;
        let Some(content) = content else { continue };
        let mut attachments = Vec::new();
        for (index, file) in content.attachments.iter().enumerate() {
            let mut path = copy_path(env, x.id, index, &file.name).filter(|p| p.is_file());
            if download && path.is_none() {
                match save(env, x.id, index).await {
                    Ok(saved) => path = Some(saved),
                    Err(e) => problems.push(format!(
                        "{} couldn't be downloaded: {}",
                        file.name,
                        e.message()
                    )),
                }
            }
            attachments.push(json!({
                "id": format!("{}/{}", x.id, index + 1),
                "name": file.name,
                "size": file.size,
                "mime_type": file.mime,
                "path": path.map(|p| p.display().to_string()),
            }));
        }
        for &f in x.folders.iter().chain(x.folder.iter()) {
            if let Some(folder) = view.folders.iter().find(|y| y.id == f && y.role.is_none()) {
                let name = view.qualified(folder);
                if !labels.contains(&name) {
                    labels.push(name);
                }
            }
        }
        let mut message = summary(&view, x);
        message["to"] = json!(content.to);
        message["cc"] = json!(content.cc);
        if !content.bcc.is_empty() {
            message["bcc"] = json!(content.bcc);
        }
        // Only when it points somewhere else than From, as Spark shows it.
        let elsewhere: Vec<&String> = content
            .reply_to
            .iter()
            .filter(|r| !r.to_lowercase().contains(&x.from_address.to_lowercase()))
            .collect();
        if !elsewhere.is_empty() {
            message["reply_to"] = json!(content.reply_to);
        }
        message["text"] = json!(cut(content.text, MAX_TEXT));
        if content.body_missing {
            message["body_missing"] = json!(true);
        }
        message["attachments"] = json!(attachments);
        messages.push(message);
    }
    let value = json!({
        "id": asked,
        "subject": members.first().map_or(m.subject.as_str(), |x| x.subject.as_str()),
        "account": view.account(m.account_id).map(|a| a.email.as_str()),
        "labels": labels,
        "messages": messages,
        "problems": problems,
    });
    let summary = format!(
        "read the thread “{}”",
        value["subject"].as_str().unwrap_or_default()
    );
    Ok(Reply::new(value.clone(), text::thread(&value)).said(summary))
}

/// Where `attachment` leaves message `id`'s file `index`.
fn copy_path(env: &Env, id: i64, index: usize, name: &str) -> Option<PathBuf> {
    Some(
        env.files
            .join(format!("{id}-{}", index + 1))
            .join(files::safe_file_name(name)),
    )
}

/// Downloads message `id`'s file `index` (the copy kept, if any) and leaves
/// a copy for spark-cli; returns its path.
async fn save(env: &Env<'_>, id: i64, index: usize) -> Result<PathBuf, Fail> {
    let file = env
        .engine
        .attachment(id, index)
        .await
        .map_err(|e| Fail::Error(format!("It couldn't be downloaded: {e}")))?;
    let path = copy_path(env, id, index, &file.name).expect("a path");
    let written = path.clone();
    tauri::async_runtime::spawn_blocking(move || files::write_opened(&written, &file.bytes))
        .await
        .map_err(|e| Fail::Error(e.to_string()))??;
    Ok(path)
}

/// `1114/2`: an email's second file, as `thread` lists it.
fn attachment_id(text: &str) -> Result<(i64, usize), Fail> {
    let bad = || {
        Fail::Error(format!(
            "{text} isn't an attachment ID. They look like 1114/2 and come from thread's Attachments."
        ))
    };
    let (message, n) = text.trim().split_once(['/', '-', ':']).ok_or_else(bad)?;
    let message = message_id(message).map_err(|_| bad())?;
    let n: usize = n.trim().parse().map_err(|_| bad())?;
    if n == 0 {
        return Err(bad());
    }
    Ok((message, n - 1))
}

pub async fn attachment(env: &Env<'_>, a: &Args<'_>) -> Done {
    let id = a
        .string("id")
        .ok_or("Pass id: an attachment ID from thread, such as 1114/2.")?;
    let (message, index) = attachment_id(&id)?;
    let (m, account) = env.message(message)?;
    let content = env
        .store()
        .content(m.id)
        .map_err(db)?
        .ok_or_else(|| super::no_message(m.id))?;
    let file = content
        .attachments
        .get(index)
        .ok_or_else(|| Fail::Error(format!("Email {message} has no attachment {}.", index + 1)))?
        .clone();
    let path = save(env, message, index).await?;
    // Read again: the grant may have gone while it downloaded.
    env.need(&account, Access::Read, "read mail")?;
    let size = std::fs::metadata(&path).map_or(file.size, |m| m.len());
    let value = json!({
        "id": format!("{message}/{}", index + 1),
        "name": file.name,
        "size": size,
        "mime_type": file.mime,
        "path": path.display().to_string(),
        "message_id": message,
    });
    let text = format!(
        "ID: {}\nName: {}\nSize: {}\nMIME Type: {}\nPath: {}\nMessage ID: {message}",
        value["id"].as_str().unwrap_or_default(),
        file.name,
        text::size(size),
        if file.mime.is_empty() {
            "unknown"
        } else {
            &file.mime
        },
        path.display()
    );
    Ok(Reply {
        targets: vec![format!("{message}/{}", index + 1)],
        summary: format!("opened {} from “{}”", file.name, m.subject),
        ..Reply::new(value, text)
    })
}

pub fn emails_word(n: usize) -> String {
    count(n, "email", "emails")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_ids_name_an_email_and_a_file_from_one() {
        assert_eq!(attachment_id("1114/2").unwrap(), (1114, 1));
        assert_eq!(attachment_id(" 7-1 ").unwrap(), (7, 0));
        assert!(attachment_id("1114").is_err());
        assert!(attachment_id("1114/0").is_err());
        assert!(attachment_id("x/1").is_err());
    }
}
