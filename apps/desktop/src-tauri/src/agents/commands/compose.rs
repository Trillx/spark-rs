//! Writing mail: `draft`, `templates` and `template`. A draft goes into the
//! account's Drafts, here and on the server, as the composer saves one;
//! `action send` sends it (`triage.rs`).

use std::collections::HashMap;
use std::path::Path;

use chrono::{Local, TimeZone};
use serde_json::{Value, json};
use spark_mail::draft::{Draft, OutgoingAttachment};
use spark_mail::signature;
use spark_store::{Account, Message, Store, Template};

use super::history::Step;
use super::{Args, Done, Env, Fail, Reply, db, message_id, refuse, text};
use crate::agents::Access;
use crate::agents::tools::Effect;
use crate::mailbox;

/// The most a file may weigh, as Spark allows.
const MAX_FILE: u64 = 25 * 1024 * 1024;
/// How many templates a page lists unless asked.
const PAGE: usize = 50;

const NO_TEAMS: &str = "spark-rs has no teams, so drafts can't be shared with teammates.";

pub async fn draft(env: &Env<'_>, a: &Args<'_>) -> Done {
    if a.str("mode") == Some("signatures") {
        return signatures(env);
    }
    if let Some(mode) = a.str("mode") {
        return Err(Fail::Error(format!(
            "draft's only mode is \"signatures\", not {mode}."
        )));
    }
    for key in [
        "team",
        "user",
        "remove_user",
        "allow_send",
        "disallow_send",
        "unshare",
    ] {
        if a.flag(key) || !a.list(key).is_empty() {
            return Err(NO_TEAMS.into());
        }
    }
    if let Some(id) = a.string("delete") {
        let others =
            a.0.as_object()
                .is_some_and(|o| o.keys().any(|k| k != "delete"));
        if others {
            return Err("delete takes no other parameters.".into());
        }
        return delete(env, message_id(&id)?);
    }
    compose(env, a).await
}

fn signatures(env: &Env) -> Done {
    let shared = env.readable()?;
    let store = env.store();
    let mut list = Vec::new();
    for (a, access) in shared {
        let signature = store.signature_for(&a.email).map_err(db)?;
        list.push(json!({
            "account": a.email,
            "access": access.name(),
            "signature": signature.map(|s| s.text),
        }));
    }
    let value = json!({ "signatures": list });
    let mut out = String::new();
    for s in value["signatures"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "{} ({})\n{}\n\n",
            s["account"].as_str().unwrap_or_default(),
            s["access"].as_str().unwrap_or_default(),
            text::indent(s["signature"].as_str().unwrap_or("(no signature)"))
        ));
    }
    Ok(Reply::new(value, out.trim_end().to_owned()).said("viewed your signatures"))
}

fn delete(env: &Env, id: i64) -> Done {
    let account = {
        let store = env.store();
        if !store.is_draft(id).map_err(db)? {
            return Err(Fail::Error(format!("Email {id} isn't a draft.")));
        }
        if store.draft_held(id).map_err(db)? {
            return Err(Fail::Error(format!(
                "Draft {id} is scheduled to send. action unschedule takes it back first."
            )));
        }
        store
            .message(id)
            .map_err(db)?
            .ok_or_else(|| super::no_message(id))?
            .account_id
    };
    let account = env.account(account)?;
    env.prefs.with_agent_access(|grants| {
        refuse(env.agent, grants, &account, Access::Triage, "delete drafts")?;
        mailbox::discard_saved_draft(env.engine, id).map_err(Fail::Error)
    })?;
    Ok(Reply {
        summary: format!("deleted draft {id}"),
        targets: vec![id.to_string()],
        effect: Some(Effect::Changed {
            summary: "deleted a draft".into(),
        }),
        ..Reply::new(
            json!({ "deleted": id, "account": account.email }),
            format!("Draft {id} deleted for good. Drafts have no Trash, so this can't be undone."),
        )
    })
}

/// What the draft answers or forwards.
enum Answer {
    Reply,
    ReplyAll,
    Forward,
}

async fn compose(env: &Env<'_>, a: &Args<'_>) -> Done {
    let edit = a.string("edit").map(|s| message_id(&s)).transpose()?;
    let reply_to = a.string("reply_to").map(|s| message_id(&s)).transpose()?;
    let reply_all = a.string("reply_all").map(|s| message_id(&s)).transpose()?;
    let forward = a.string("forward").map(|s| message_id(&s)).transpose()?;
    if reply_to.is_some() && reply_all.is_some() {
        return Err("reply_to and reply_all can't go together: reply_all answers everyone, reply_to the sender alone.".into());
    }
    let answering = match (reply_to, reply_all, forward) {
        (Some(id), None, None) => Some((Answer::Reply, id)),
        (None, Some(id), None) => Some((Answer::ReplyAll, id)),
        (None, None, Some(id)) => Some((Answer::Forward, id)),
        (None, None, None) => None,
        _ => return Err("Pass only one of reply_to, reply_all and forward.".into()),
    };
    if edit.is_some() && answering.is_some() {
        return Err(
            "edit changes a draft that exists; reply_to, reply_all and forward start a new one."
                .into(),
        );
    }
    let template = match a.string("template") {
        Some(name) => Some(find_template(&env.store(), &name)?),
        None => None,
    };
    let body = a.raw("body").filter(|b| !b.trim().is_empty());
    if edit.is_none() && body.is_none() && template.is_none() && answering.is_none() {
        return Err("Pass body: the email's text, in markdown.".into());
    }
    let mut warnings = Vec::new();

    // Where it starts: the draft being edited, or a new one.
    let (mut draft, original) = match edit {
        Some(id) => {
            let (_, account) = env.message(id)?;
            if !env.store().is_draft(id).map_err(db)? {
                return Err(Fail::Error(format!("Email {id} isn't a draft.")));
            }
            env.need(&account, Access::Triage, "write drafts")?;
            let draft = mailbox::open_live(env.engine, id)
                .await
                .map_err(Fail::Error)?;
            (draft, None)
        }
        None => {
            let original = match &answering {
                Some((_, id)) => {
                    let (m, _) = env.message(*id)?;
                    // Older mail may be stored without its body: a forward
                    // would quote only its preview, Reply All miss its Cc.
                    if env.store().body_missing(m.id).map_err(db)? {
                        env.engine.fetch_body(m.id).await.map_err(|e| {
                            Fail::Error(format!("Email {} couldn't be downloaded first: {e}", m.id))
                        })?;
                    }
                    Some(m)
                }
                None => None,
            };
            let account = match (a.str("account"), &original) {
                (Some(name), _) => env.account_named(name)?,
                (None, Some(m)) => env.account(m.account_id)?,
                (None, None) => first_writable(env)?,
            };
            let draft = Draft {
                account: account.id,
                ..Draft::default()
            };
            (draft, original)
        }
    };
    if edit.is_some()
        && let Some(name) = a.str("account")
    {
        draft.account = env.account_named(name)?.id;
    }
    let account = env.account(draft.account)?;
    env.need(&account, Access::Triage, "write drafts")?;

    if let (Some((kind, _)), Some(m)) = (&answering, &original) {
        answer(env, &mut draft, kind, m, &account)?;
    }

    // Recipients and subject the agent gave win over what answering made.
    let list = |key: &str| a.addresses(key).join(", ");
    for (key, field) in [
        ("to", &mut draft.to),
        ("cc", &mut draft.cc),
        ("bcc", &mut draft.bcc),
    ] {
        let given = list(key);
        if !given.is_empty() {
            *field = given;
        }
    }
    for field in [&draft.to, &draft.cc, &draft.bcc] {
        spark_mail::draft::recipients(field).map_err(|e| Fail::Error(e.to_string()))?;
    }
    if let Some(subject) = a.str("subject") {
        draft.subject = subject.to_owned();
    }

    if let Some(t) = &template {
        if a.str("subject").is_none() {
            draft.subject = t.subject.clone();
        }
        if body.is_none() {
            let filled = fill(t, &draft, &account, &a.list("placeholder"))?;
            warnings.extend(filled.warnings);
            draft.body = signature::body_text(&filled.html);
            draft.body_html = Some(filled.html);
        }
        env.store()
            .template_used(t.id, spark_store::now_ms())
            .map_err(db)?;
    }
    if let Some(body) = body {
        draft.body_html = Some(markdown(body));
        draft.body = body.trim_end().to_owned();
    }

    // The signature: the account's own, added as the composer adds it.
    if a.flag("no_signature") {
        draft.signature = None;
        draft.signature_id = None;
    } else if edit.is_none() {
        let store = env.store();
        let chosen = match &original {
            Some(m) if !matches!(answering, Some((Answer::Forward, _))) => {
                signature::for_reply(&store, &account.email, m.id)
            }
            _ => store.signature_for(&account.email),
        }
        .map_err(db)?;
        draft.signature_id = chosen.as_ref().map(|s| s.id);
        draft.signature = chosen.map(|s| s.html);
    }

    for path in a.list("attach") {
        draft.attachments.push(local_file(&path)?);
    }
    for id in a.list("attach_id") {
        draft.attachments.push(message_file(env, &id)?);
    }
    draft.check_size().map_err(|e| Fail::Error(e.to_string()))?;
    if draft.to.trim().is_empty() && draft.cc.trim().is_empty() && draft.bcc.trim().is_empty() {
        warnings.push("It has no recipients yet: pass to before sending it.".to_owned());
    }
    if draft.subject.trim().is_empty() {
        warnings.push("It has no subject yet.".to_owned());
    }

    // Saved while the grants can't change, as every write here is.
    let saved = env.prefs.with_agent_access(|grants| {
        refuse(env.agent, grants, &account, Access::Triage, "write drafts")?;
        mailbox::save_saved_draft_if(
            env.engine,
            &draft,
            |_| Ok(()),
            |store, id| store.auto_draft_edited(id).map_err(|e| e.to_string()),
        )
        .map_err(Fail::Error)
    })?;
    let saved_at = env.store().draft_saved_at(saved).map_err(db)?;

    let composed = composed_body(&draft);
    let files: Vec<&str> = draft
        .attachments
        .iter()
        .map(|f| match f {
            OutgoingAttachment::File { name, .. } | OutgoingAttachment::Message { name, .. } => {
                name.as_str()
            }
        })
        .collect();
    let from = if account.name.trim().is_empty() {
        account.email.clone()
    } else {
        format!("{} <{}>", account.name.trim(), account.email)
    };
    let value = json!({
        "id": saved,
        "account": account.email,
        "from": from,
        "to": split(&draft.to),
        "cc": split(&draft.cc),
        "bcc": split(&draft.bcc),
        "subject": draft.subject,
        "reply_to": draft.reply_to,
        "attachments": files,
        "body": composed,
        "warnings": warnings,
        "note": format!("It's in {}'s Drafts. action send {saved} sends it, with send access.", account.email),
    });
    let verb = if edit.is_some() { "updated" } else { "saved" };
    let mut out = format!(
        "Draft {saved} {verb} in {}'s Drafts.\nID: {saved}\nFrom: {from}\n",
        account.email
    );
    for (label, field) in [("To", &draft.to), ("Cc", &draft.cc), ("Bcc", &draft.bcc)] {
        if !field.trim().is_empty() {
            out.push_str(&format!("{label}: {field}\n"));
        }
    }
    out.push_str(&format!("Subject: {}\n", draft.subject));
    if !files.is_empty() {
        out.push_str(&format!("Attachments: {}\n", files.join(", ")));
    }
    for w in &warnings {
        out.push_str(&format!("Warning: {w}\n"));
    }
    out.push_str(&format!("Body:\n{}", text::indent(&composed)));
    let undo = match edit {
        // An edit isn't undone; a draft the agent wrote is deleted.
        Some(_) => Vec::new(),
        None => vec![Step::Draft {
            id: saved,
            saved_at,
        }],
    };
    Ok(Reply {
        summary: match (&answering, &original, edit) {
            (_, _, Some(_)) => format!("edited the draft “{}”", draft.subject),
            (Some((Answer::Forward, _)), Some(m), _) => {
                format!("drafted a forward of “{}”", m.subject)
            }
            (Some(_), Some(m), _) => format!("drafted a reply to “{}”", m.subject),
            _ => format!("drafted “{}”", draft.subject),
        },
        targets: vec![saved.to_string()],
        undo,
        effect: Some(Effect::Changed {
            summary: if edit.is_some() {
                "edited a draft".into()
            } else {
                "wrote a draft".into()
            },
        }),
        ..Reply::new(value, out)
    })
}

/// The first account agents may write drafts in.
fn first_writable(env: &Env) -> Result<Account, Fail> {
    env.readable()?
        .into_iter()
        .find(|(_, access)| *access >= Access::Triage)
        .map(|(a, _)| a)
        .ok_or_else(|| {
            Fail::Blocked(
                "No account lets agents write drafts. The user can allow triage in spark-rs > Settings > Agents."
                    .into(),
            )
        })
}

/// Fills in a reply's or forward's recipients, subject, threading and
/// quote, as the app's Reply, Reply All and Forward do.
fn answer(
    env: &Env,
    draft: &mut Draft,
    kind: &Answer,
    m: &Message,
    account: &Account,
) -> Result<(), Fail> {
    let content = env
        .store()
        .content(m.id)
        .map_err(db)?
        .ok_or_else(|| super::no_message(m.id))?;
    draft.reply_to = Some(m.id);
    match kind {
        Answer::Reply | Answer::ReplyAll => {
            let (to, cc) = recipients(
                m,
                &content,
                &account.email,
                matches!(kind, Answer::ReplyAll),
            );
            draft.to = to.join(", ");
            draft.cc = cc.join(", ");
            draft.subject = if m.subject.to_lowercase().starts_with("re:") {
                m.subject.clone()
            } else {
                format!("Re: {}", m.subject)
            };
        }
        Answer::Forward => {
            let lower = m.subject.to_lowercase();
            draft.subject = if lower.starts_with("fwd:") || lower.starts_with("fw:") {
                m.subject.clone()
            } else {
                format!("Fwd: {}", m.subject)
            };
            let to: Vec<&String> = content.to.iter().filter(|t| *t != "You").collect();
            let date = Local
                .timestamp_millis_opt(m.date)
                .single()
                .map(|t| t.format("%a, %-d %b %Y at %H:%M").to_string())
                .unwrap_or_default();
            let mut header = vec![
                "---------- Forwarded message ----------".to_owned(),
                format!("From: {} <{}>", m.from_name, m.from_address),
                format!("Date: {date}"),
                format!("Subject: {}", m.subject),
            ];
            if !to.is_empty() {
                header.push(format!(
                    "To: {}",
                    to.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                ));
            }
            draft.quote = Some(format!("{}\n\n{}", header.join("\n"), content.text));
            draft.attachments = content
                .attachments
                .iter()
                .enumerate()
                .map(|(index, a)| OutgoingAttachment::Message {
                    message: m.id,
                    index,
                    name: a.name.clone(),
                    size: a.size,
                })
                .collect();
        }
    }
    Ok(())
}

/// Who a reply goes to: the Reply-To addresses, else the sender; on your
/// own email, its recipients. Reply All adds everyone else on To and Cc,
/// leaving out your own address.
fn recipients(
    m: &Message,
    content: &spark_store::Content,
    own: &str,
    all: bool,
) -> (Vec<String>, Vec<String>) {
    let own = own.trim().to_lowercase();
    let sender = m.from_address.to_lowercase();
    let from_you = sender == own;
    let mut seen: Vec<String> = vec![String::new(), "you".into(), own.clone()];
    let mut take = |list: &[String], exclude_sender: bool| -> Vec<String> {
        let mut out = Vec::new();
        for entry in list {
            let address = address_of(entry).to_lowercase();
            if seen.contains(&address) || (exclude_sender && address == sender) {
                continue;
            }
            seen.push(address);
            out.push(entry.clone());
        }
        out
    };
    let from = if m.from_name.trim().is_empty() {
        m.from_address.clone()
    } else {
        format!("{} <{}>", m.from_name.trim(), m.from_address)
    };
    let targets = if content.reply_to.is_empty() {
        vec![from]
    } else {
        content.reply_to.clone()
    };
    let mut to = take(if from_you { &content.to } else { &targets }, false);
    if all && !from_you {
        to.extend(take(&content.to, true));
    }
    let cc = if all {
        take(&content.cc, true)
    } else {
        Vec::new()
    };
    (to, cc)
}

/// The address in `Name <address>`, or the entry itself.
fn address_of(entry: &str) -> &str {
    match (entry.rfind('<'), entry.rfind('>')) {
        (Some(start), Some(end)) if start < end => entry[start + 1..end].trim(),
        _ => entry.trim(),
    }
}

fn split(field: &str) -> Vec<String> {
    spark_mail::draft::recipients(field)
        .map(|list| {
            list.into_iter()
                .map(|m| match &m.name {
                    Some(name) if !name.is_empty() => format!("{name} <{}>", m.email),
                    _ => m.email.to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The body as the recipient reads it: the text, then the signature, without
/// the quoted email below.
fn composed_body(draft: &Draft) -> String {
    let mut out = draft.body.trim_end().to_owned();
    if let Some(html) = &draft.signature {
        let sig = signature::text(html);
        if !sig.trim().is_empty() {
            out.push_str("\n\n");
            out.push_str(sig.trim_end());
        }
    }
    out
}

/// Markdown to the HTML the composer would have written.
pub fn markdown(text: &str) -> String {
    use pulldown_cmark::{Options, Parser, html};
    let options =
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;
    let mut out = String::new();
    html::push_html(&mut out, Parser::new_ext(text, options));
    out
}

fn local_file(path: &str) -> Result<OutgoingAttachment, Fail> {
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(Fail::Error(format!(
            "{path} isn't an absolute path. spark-rs reads files where they are, so give the whole path."
        )));
    }
    let meta = std::fs::metadata(p)
        .map_err(|e| Fail::Error(format!("spark-rs can't read {path}: {e}")))?;
    if !meta.is_file() {
        return Err(Fail::Error(format!("{path} isn't a file.")));
    }
    if meta.len() > MAX_FILE {
        return Err(Fail::Error(format!(
            "{path} is over 25 MB, more than mail servers take."
        )));
    }
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "attachment".into());
    Ok(OutgoingAttachment::File {
        path: path.to_owned(),
        name,
        size: meta.len(),
    })
}

/// A file of a stored email (`1114/2`), sent again from its server.
fn message_file(env: &Env, id: &str) -> Result<OutgoingAttachment, Fail> {
    let bad = || {
        Fail::Error(format!(
            "{id} isn't an attachment ID such as 1114/2 (from thread)."
        ))
    };
    let (message, n) = id.trim().split_once(['/', '-', ':']).ok_or_else(bad)?;
    let message = message_id(message).map_err(|_| bad())?;
    let n: usize = n.trim().parse().ok().filter(|&n| n > 0).ok_or_else(bad)?;
    let (m, _) = env.message(message)?;
    let content = env
        .store()
        .content(m.id)
        .map_err(db)?
        .ok_or_else(|| super::no_message(m.id))?;
    let file = content
        .attachments
        .get(n - 1)
        .ok_or_else(|| Fail::Error(format!("Email {message} has no attachment {n}.")))?;
    Ok(OutgoingAttachment::Message {
        message,
        index: n - 1,
        name: file.name.clone(),
        size: file.size,
    })
}

// ---- Templates ----

pub fn templates(env: &Env, a: &Args) -> Done {
    env.readable()?;
    if a.str("team").is_some() {
        return Err("spark-rs has no teams; its templates are all personal.".into());
    }
    let (page, size) = a.paging(PAGE)?;
    let all = env.store().templates().map_err(db)?;
    let total = all.len();
    let list: Vec<Value> = all
        .iter()
        .skip((page - 1) * size)
        .take(size)
        .map(|t| {
            json!({
                "id": t.id,
                "scope": "Personal",
                "name": t.name,
                "subject": t.subject,
                "last_used": t.used_at.map(super::iso),
            })
        })
        .collect();
    let value = json!({ "page": page, "page_size": size, "total": total, "templates": list });
    Ok(Reply::new(value.clone(), text::templates(&value)).said("viewed your templates"))
}

pub fn template(env: &Env, a: &Args) -> Done {
    env.readable()?;
    let name = a
        .string("ref")
        .or_else(|| a.string("id"))
        .ok_or("Pass ref: a template's ID or name, from templates.")?;
    let t = find_template(&env.store(), &name)?;
    let placeholders: Vec<Value> = chips(&t.html)
        .into_iter()
        .map(|c| {
            json!({
                "name": c.label,
                "kind": if c.kind == "custom" { "manual" } else { "auto" },
                "fills": c.kind,
            })
        })
        .collect();
    let body = signature::body_text(&t.html);
    let value = json!({
        "id": t.id,
        "scope": "Personal",
        "name": t.name,
        "subject": t.subject,
        "body": body,
        "placeholders": placeholders,
        "last_used": t.used_at.map(super::iso),
    });
    let mut out = format!(
        "ID: {}\nScope: Personal\nName: {}\nSubject: {}\nBody:\n{}\n",
        t.id,
        t.name,
        t.subject,
        text::indent(&body)
    );
    if placeholders.is_empty() {
        out.push_str("Placeholders: none");
    } else {
        out.push_str("Placeholders:");
        for p in &placeholders {
            out.push_str(&format!(
                "\n  [{}] {}",
                p["kind"].as_str().unwrap_or_default(),
                p["name"].as_str().unwrap_or_default()
            ));
        }
    }
    Ok(Reply::new(value, out).said(format!("opened the template “{}”", t.name)))
}

/// A template by ID, or by name ignoring case.
fn find_template(store: &Store, name: &str) -> Result<Template, Fail> {
    let all = store.templates().map_err(db)?;
    if let Ok(id) = name.trim().parse::<i64>()
        && let Some(t) = all.iter().find(|t| t.id == id)
    {
        return Ok(t.clone());
    }
    let wanted = name.trim().to_lowercase();
    let found: Vec<&Template> = all
        .iter()
        .filter(|t| t.name.trim().to_lowercase() == wanted)
        .collect();
    match found.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(Fail::Error(format!(
            "There's no template {name}. templates lists them."
        ))),
        many => Err(Fail::Error(format!(
            "More than one template is called {name}: IDs {}. Pass one of them.",
            many.iter()
                .map(|t| t.id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// A placeholder in a template's HTML, as the editor writes it:
/// `<span data-placeholder="kind" data-label="Label" …>Label</span>`.
#[derive(Debug, PartialEq)]
struct Chip {
    start: usize,
    end: usize,
    kind: String,
    label: String,
}

fn chips(html: &str) -> Vec<Chip> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(i) = html[pos..].find("<span") {
        let start = pos + i;
        let Some(close) = html[start..].find('>') else {
            break;
        };
        let tag = &html[start..start + close];
        pos = start + close + 1;
        let Some(kind) = attribute(tag, "data-placeholder") else {
            continue;
        };
        let Some(end) = html[pos..].find("</span>") else {
            break;
        };
        let inner = &html[pos..pos + end];
        let label = attribute(tag, "data-label").unwrap_or_else(|| unescape(inner));
        let end = pos + end + "</span>".len();
        out.push(Chip {
            start,
            end,
            kind,
            label,
        });
        pos = end;
    }
    out
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    let at = tag.find(&format!("{name}=\""))? + name.len() + 2;
    let len = tag[at..].find('"')?;
    Some(unescape(&tag[at..at + len]))
}

fn unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

struct Filled {
    html: String,
    warnings: Vec<String>,
}

/// The template's HTML with its placeholders filled: the recipient's and
/// your name from the draft, the others from `given` (`Name=value`). One
/// left out fails before anything is saved.
fn fill(t: &Template, draft: &Draft, account: &Account, given: &[String]) -> Result<Filled, Fail> {
    let mut values: HashMap<String, String> = HashMap::new();
    for pair in given {
        let (name, value) = pair.split_once('=').ok_or_else(|| {
            Fail::Error(format!(
                "{pair} isn't a placeholder value: write \"Name=value\"."
            ))
        })?;
        values.insert(name.trim().to_lowercase(), value.trim().to_owned());
    }
    let recipient = split(&draft.to)
        .first()
        .map(|r| match r.rfind('<') {
            Some(at) => r[..at].trim().trim_matches('"').to_owned(),
            None => String::new(),
        })
        .unwrap_or_default();
    let them = name_parts(&recipient);
    let me = name_parts(&account.name);
    let chips = chips(&t.html);
    let missing: Vec<&str> = chips
        .iter()
        .filter(|c| c.kind == "custom" && !values.contains_key(&c.label.to_lowercase()))
        .map(|c| c.label.as_str())
        .collect();
    if !missing.is_empty() {
        return Err(Fail::Error(format!(
            "Template {} needs placeholder {}: pass placeholder \"Name=value\" for each.",
            t.name,
            missing.join(", ")
        )));
    }
    let mut html = String::new();
    let mut last = 0;
    let mut warnings = Vec::new();
    for c in &chips {
        let value = match c.kind.as_str() {
            "custom" => values.get(&c.label.to_lowercase()).cloned(),
            "recipientFirstName" => Some(them.0.clone()),
            "recipientLastName" => Some(them.1.clone()),
            "recipientFullName" => Some(them.2.clone()),
            "myFirstName" => Some(me.0.clone()),
            "myLastName" => Some(me.1.clone()),
            "myFullName" => Some(me.2.clone()),
            _ => None,
        }
        .filter(|v| !v.is_empty());
        html.push_str(&t.html[last..c.start]);
        match value {
            Some(v) => html.push_str(&escape(&v)),
            None => {
                // Left for the user to fill in the composer.
                html.push_str(&t.html[c.start..c.end]);
                warnings.push(format!("{} is still to fill in.", c.label));
            }
        }
        last = c.end;
    }
    html.push_str(&t.html[last..]);
    Ok(Filled { html, warnings })
}

/// A name's first, last and full forms: "Ana Ruiz", or "Ruiz, Ana".
fn name_parts(name: &str) -> (String, String, String) {
    let full = name.trim().trim_matches(['"', '\'']).trim();
    let comma: Vec<&str> = full.split(',').map(str::trim).collect();
    if comma.len() == 2 && !comma[0].is_empty() && !comma[1].is_empty() {
        let first = comma[1].split_whitespace().next().unwrap_or_default();
        return (
            first.into(),
            comma[0].into(),
            format!("{} {}", comma[1], comma[0]),
        );
    }
    let words: Vec<&str> = full.split_whitespace().collect();
    let first = words.first().copied().unwrap_or_default();
    let last = if words.len() > 1 {
        words[words.len() - 1]
    } else {
        ""
    };
    (first.into(), last.into(), full.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chips_are_found_with_their_kind_and_label() {
        let html = r#"<p>Hi <span data-placeholder="recipientFirstName" data-label="Recipient's first name" contenteditable="false">Recipient's first name</span>, re <span data-placeholder="custom" data-label="Project &amp; phase" contenteditable="false">Project &amp; phase</span>.</p>"#;
        let found = chips(html);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].kind, "recipientFirstName");
        assert_eq!(found[1].label, "Project & phase");
        assert_eq!(&html[found[1].end..], ".</p>");
    }

    #[test]
    fn names_split_both_ways() {
        assert_eq!(name_parts("Ana Ruiz").0, "Ana");
        assert_eq!(
            name_parts("Ruiz, Ana"),
            ("Ana".into(), "Ruiz".into(), "Ana Ruiz".into())
        );
        assert_eq!(name_parts("Cher").1, "");
    }

    #[test]
    fn markdown_becomes_html() {
        let html = markdown("Hi **Ana**,\n\n- one\n- two");
        assert!(html.contains("<strong>Ana</strong>"));
        assert!(html.contains("<li>one</li>"));
    }

    #[test]
    fn addresses_come_out_of_entries() {
        assert_eq!(address_of("Ana Ruiz <ana@example.com>"), "ana@example.com");
        assert_eq!(address_of(" bo@example.com "), "bo@example.com");
    }
}
