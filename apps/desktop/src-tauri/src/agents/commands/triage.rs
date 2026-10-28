//! Changing mail and senders: `action`, `contact-action`, and the
//! `contacts` and `contact` they look people up with.

use std::collections::HashSet;

use serde_json::{Value, json};
use spark_mail::QueuedChange;
use spark_mail::draft::SendLater;
use spark_mail::unsubscribe::Unsubscribed;
use spark_store::{
    Category, Change, Decision, FolderRole, Message, MessageState, NewReminder, Patch, Screen,
    domain_of, now_ms,
};

use super::filter::{List, View};
use super::history::{Snapshot, Step};
use super::{
    Args, Done, Env, Fail, Reply, capitalize, count, db, iso, message_id, parse_time, refuse, text,
    when,
};
use crate::agents::Access;
use crate::agents::tools::{Effect, SEND_DELAY_MS};
use crate::mailbox;

const NO_TEAMS: &str = "spark-rs has no teams, so there's no one to share with or assign to.";

/// How the messages an action names map to the ones it changes, as the
/// app's toolbar does it.
#[derive(Clone, Copy)]
enum Reach {
    /// The email itself.
    Own,
    /// Its conversation's emails in the same place, which move together.
    Together,
    /// Its conversation's emails that have the flag, to clear it.
    Flagged(fn(&Message) -> bool),
}

pub async fn action(env: &Env<'_>, a: &Args<'_>) -> Done {
    let verb = a
        .str("action_name")
        .or_else(|| a.str("action"))
        .ok_or("Pass action_name, e.g. archive, pin or snooze.")?
        .to_owned();
    let mut ids = a.list("message_ids");
    ids.extend(a.list("message_id"));
    if ids.is_empty() {
        return Err("Pass message_ids: the emails to act on, from emails or search.".into());
    }
    match verb.as_str() {
        "send" => return send(env, a, &ids).await,
        "unschedule" => return unschedule(env, &ids).await,
        "unsubscribe" => return unsubscribe(env, &ids).await,
        "shareInTeam" | "assign" | "delegationComplete" | "delegationReopen" => {
            return Err(NO_TEAMS.into());
        }
        "attachLabel" | "detachLabel" => {
            return Err(
                "spark-rs can move mail to a label or folder (moveToFolder) but can't add or remove one label alone yet."
                    .into(),
            );
        }
        _ => {}
    }
    let ids: Vec<i64> = ids
        .iter()
        .map(|id| message_id(id))
        .collect::<Result<_, _>>()?;
    let view = {
        let accounts = env.readable()?.into_iter().map(|(a, _)| a).collect();
        View::read(&env.store(), accounts)?
    };
    let named: Vec<&Message> = ids
        .iter()
        .map(|&id| {
            view.messages
                .iter()
                .find(|m| m.id == id)
                .ok_or_else(|| super::no_message(id))
        })
        .collect::<Result<_, _>>()?;
    let accounts: Vec<i64> = named
        .iter()
        .map(|m| m.account_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    match verb.as_str() {
        "mute" | "unmute" => return mute(env, &named, &accounts, verb == "mute"),
        "changeCategoryPersonal" | "changeCategoryNotification" | "changeCategoryNewsletters" => {
            let senders: Vec<String> = named.iter().map(|m| m.from_address.clone()).collect();
            return categorize(env, &verb, &senders);
        }
        "changeReminder" | "clearReminder" => {
            return remind(env, a, &view, &named, verb == "changeReminder");
        }
        _ => {}
    }
    let state = |s: MessageState| Patch {
        state: Some(s),
        ..Patch::default()
    };
    let (patch, reach, done): (Patch, Reach, String) = match verb.as_str() {
        "pin" => (
            Patch {
                pinned: Some(true),
                ..Patch::default()
            },
            Reach::Own,
            "pinned {}".to_owned(),
        ),
        "unpin" => (
            Patch {
                pinned: Some(false),
                ..Patch::default()
            },
            Reach::Flagged(|m| m.pinned),
            "unpinned {}".to_owned(),
        ),
        "markThreadAsPriority" => (
            Patch {
                priority: Some(true),
                ..Patch::default()
            },
            Reach::Own,
            "marked {} as Priority".to_owned(),
        ),
        "unmarkThreadAsPriority" => (
            Patch {
                priority: Some(false),
                ..Patch::default()
            },
            Reach::Flagged(|m| m.priority),
            "removed Priority from {}".to_owned(),
        ),
        "markAsSeen" => (
            Patch {
                unread: Some(false),
                ..Patch::default()
            },
            Reach::Flagged(|m| m.unread),
            "marked {} as read".to_owned(),
        ),
        "markAsUnseen" => (
            Patch {
                unread: Some(true),
                ..Patch::default()
            },
            Reach::Own,
            "marked {} as unread".to_owned(),
        ),
        "archive" => (
            state(MessageState::Done),
            Reach::Together,
            "archived {}".to_owned(),
        ),
        "markAsDone" => (
            state(MessageState::Done),
            Reach::Together,
            "marked {} as Done".to_owned(),
        ),
        "markAsUndone" => (
            state(MessageState::Inbox),
            Reach::Together,
            "marked {} as not Done".to_owned(),
        ),
        "moveToInbox" => (
            state(MessageState::Inbox),
            Reach::Together,
            "moved {} to the Inbox".to_owned(),
        ),
        "unsnooze" => (
            state(MessageState::Inbox),
            Reach::Together,
            "unsnoozed {}".to_owned(),
        ),
        "setAside" => (
            state(MessageState::Aside),
            Reach::Together,
            "set {} aside".to_owned(),
        ),
        "moveToTrash" => (
            state(MessageState::Deleted),
            Reach::Together,
            "moved {} to the Trash".to_owned(),
        ),
        "snooze" => {
            let date = a
                .str("date")
                .ok_or("Pass date: when they come back, e.g. 2026-04-10T09:00.")?;
            let until = parse_time(date, false)?;
            if until <= now_ms() {
                return Err(Fail::Error(format!(
                    "{date} has passed; snooze until a time to come."
                )));
            }
            (
                Patch {
                    state: Some(MessageState::Snoozed),
                    snoozed_until: Some(until),
                    ..Patch::default()
                },
                Reach::Together,
                format!("snoozed {{}} until {}", when(until)),
            )
        }
        "moveToFolder" | "markAsSpam" => {
            return file(env, a, &view, &named, &accounts, verb == "markAsSpam");
        }
        other => {
            return Err(Fail::Error(format!(
                "spark-rs doesn't know the action {other}. emails actions are: pin, unpin, mute, unmute, snooze, unsnooze, changeReminder, clearReminder, setAside, archive, moveToInbox, moveToTrash, moveToFolder, markAsDone, markAsUndone, markAsSeen, markAsUnseen, markAsSpam, markThreadAsPriority, unmarkThreadAsPriority, unsubscribe, changeCategoryPersonal, changeCategoryNotification, changeCategoryNewsletters, send, unschedule."
            )));
        }
    };
    if verb == "unsnooze" && named.iter().any(|m| m.state != MessageState::Snoozed) {
        return Err("unsnooze takes snoozed emails; moveToInbox moves others.".into());
    }
    let targets = reach_out(&view, &named, reach);
    let patches: Vec<(i64, Patch)> = targets.iter().map(|&id| (id, patch.clone())).collect();
    apply(env, &accounts, &patches, &done, &what(&named))
}

/// The emails an action names, for its summary: "“Subject”", or "3 emails".
fn what(named: &[&Message]) -> String {
    match named {
        [one] => format!("“{}”", one.subject),
        many => super::mail::emails_word(many.len()),
    }
}

/// The emails an action changes, each once.
fn reach_out(view: &View, named: &[&Message], reach: Reach) -> Vec<i64> {
    let mut out: Vec<i64> = Vec::new();
    for m in named {
        let conversation = || {
            view.messages.iter().filter(move |x| {
                x.conversation == m.conversation
                    && x.account_id == m.account_id
                    && (x.id == m.id
                        || !matches!(x.state, MessageState::Deleted | MessageState::Blocked)
                        || x.state == m.state)
            })
        };
        let picked: Vec<i64> = match reach {
            Reach::Own => vec![m.id],
            Reach::Together => conversation()
                .filter(|x| {
                    x.state == m.state && x.folder == m.folder && x.screening == m.screening
                })
                .map(|x| x.id)
                .collect(),
            Reach::Flagged(flag) => conversation().filter(|x| flag(x)).map(|x| x.id).collect(),
        };
        for id in picked {
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

/// Changes each email as patched, after checking every account allows it,
/// and records how to undo it. `done` says what happened, with `{}` for
/// the emails the agent named (`what`).
fn apply(env: &Env, accounts: &[i64], patches: &[(i64, Patch)], done: &str, what: &str) -> Done {
    let now = now_ms();
    let undo = env.guarded(accounts, Access::Triage, "change mail", |store| {
        let mut undo = Vec::new();
        for (id, patch) in patches {
            let Some(before) = store.message(*id).map_err(db)? else {
                continue;
            };
            if let Some(after) = store.update(*id, patch, now).map_err(db)? {
                undo.push(Step::Message {
                    id: *id,
                    before: Snapshot::of(&before),
                    after: Snapshot::of(&after),
                });
            }
        }
        Ok(undo)
    })?;
    let summary = done.replace("{}", what);
    let ids: Vec<i64> = patches.iter().map(|(id, _)| *id).collect();
    Ok(Reply {
        targets: ids.iter().map(i64::to_string).collect(),
        summary: summary.clone(),
        undo,
        effect: Some(Effect::Changed {
            summary: summary.clone(),
        }),
        ..Reply::new(
            json!({ "changed": ids, "note": "Changes reach the server after a few seconds." }),
            format!("{}.", capitalize(&summary)),
        )
    })
}

/// `moveToFolder` and `markAsSpam`: files each conversation into a folder
/// of its own account.
fn file(
    env: &Env,
    a: &Args,
    view: &View,
    named: &[&Message],
    accounts: &[i64],
    spam: bool,
) -> Done {
    let target = if spam {
        None
    } else {
        Some(a.str("folder").ok_or(
            "Pass folder: where to move them, e.g. user@example.com:Projects (folders lists them).",
        )?)
    };
    let mut patches = Vec::new();
    for m in named {
        let account = view
            .account(m.account_id)
            .ok_or_else(|| super::no_message(m.id))?;
        let patch = match target {
            None => {
                let folder = view
                    .role_folder(m.account_id, FolderRole::Spam)
                    .ok_or_else(|| {
                        Fail::Error(format!(
                            "{} has no Spam folder spark-rs knows of yet.",
                            account.email
                        ))
                    })?;
                Patch {
                    folder: Some(folder.id),
                    ..Patch::default()
                }
            }
            Some(name) => {
                // A bare folder name means the email's own account.
                let qualified = if name.contains(':')
                    && name.split(':').next().is_some_and(|w| w.contains('@'))
                {
                    name.to_owned()
                } else {
                    format!("{}:{name}", account.email)
                };
                let scope =
                    view.scope(Some(&qualified), super::filter::Place::List(List::Inbox))?;
                if !scope.accounts.contains(&m.account_id) {
                    return Err(Fail::Error(format!(
                        "Email {} is in {}; it can only move to that account's folders.",
                        m.id, account.email
                    )));
                }
                match scope.place {
                    super::filter::Place::List(List::Inbox) => Patch {
                        state: Some(MessageState::Inbox),
                        ..Patch::default()
                    },
                    super::filter::Place::List(List::Done) => Patch {
                        state: Some(MessageState::Done),
                        ..Patch::default()
                    },
                    super::filter::Place::List(List::Trash) => Patch {
                        state: Some(MessageState::Deleted),
                        ..Patch::default()
                    },
                    super::filter::Place::List(List::SetAside) => Patch {
                        state: Some(MessageState::Aside),
                        ..Patch::default()
                    },
                    super::filter::Place::List(list @ (List::Spam | List::Sent | List::Drafts)) => {
                        let role = match list {
                            List::Spam => FolderRole::Spam,
                            List::Sent => FolderRole::Sent,
                            _ => FolderRole::Drafts,
                        };
                        let folder = view.role_folder(m.account_id, role).ok_or_else(|| {
                            Fail::Error(format!(
                                "{} has no {} folder spark-rs knows of yet.",
                                account.email,
                                list.name()
                            ))
                        })?;
                        Patch {
                            folder: Some(folder.id),
                            ..Patch::default()
                        }
                    }
                    super::filter::Place::Folders(ids) if ids.len() == 1 => Patch {
                        folder: Some(ids[0]),
                        ..Patch::default()
                    },
                    super::filter::Place::Folders(_) => {
                        return Err(Fail::Error(format!(
                            "More than one folder is called {name}; give its whole path, e.g. Parent/{name}."
                        )));
                    }
                    _ => return Err(Fail::Error(format!("Mail can't be moved to {name}."))),
                }
            }
        };
        for id in reach_out(view, &[m], Reach::Together) {
            patches.push((id, patch.clone()));
        }
    }
    let done = if spam {
        "marked {} as spam".to_owned()
    } else {
        format!("moved {{}} to {}", target.unwrap_or_default())
    };
    apply(env, accounts, &patches, &done, &what(named))
}

fn mute(env: &Env, named: &[&Message], accounts: &[i64], muted: bool) -> Done {
    let undo = env.guarded(accounts, Access::Triage, "change mail", |store| {
        let mut undo = Vec::new();
        let mut done = HashSet::new();
        for m in named {
            if done.insert(m.conversation) {
                store.set_muted(m.conversation, muted).map_err(db)?;
                undo.push(Step::Muted {
                    conversation: m.conversation,
                    before: m.muted,
                });
            }
        }
        Ok(undo)
    })?;
    let summary = format!(
        "{} {}",
        if muted { "muted" } else { "unmuted" },
        count(undo.len(), "conversation", "conversations")
    );
    Ok(Reply {
        targets: named.iter().map(|m| m.id.to_string()).collect(),
        summary: summary.clone(),
        undo,
        effect: Some(Effect::Changed {
            summary: summary.clone(),
        }),
        ..Reply::new(
            json!({ "changed": named.iter().map(|m| m.id).collect::<Vec<_>>() }),
            format!("{}.", capitalize(&summary)),
        )
    })
}

/// Files senders' mail under a category, now and later, in every account.
fn categorize(env: &Env, verb: &str, senders: &[String]) -> Done {
    let category = match verb {
        "changeCategoryPersonal" => Category::People,
        "changeCategoryNotification" => Category::Notifications,
        _ => Category::Newsletters,
    };
    let all = all_accounts(env)?;
    let mut unique: Vec<String> = Vec::new();
    for s in senders {
        let s = s.trim().to_lowercase();
        if domain_of(&s).is_none() {
            return Err(Fail::Error(format!("{s} isn't an email address.")));
        }
        if !unique.contains(&s) {
            unique.push(s);
        }
    }
    let undo = env.guarded(
        &all,
        Access::Triage,
        "change how a sender's mail is sorted",
        |store| {
            let mut undo = Vec::new();
            for s in &unique {
                let before = store.set_sender_category(s, Some(category)).map_err(db)?;
                undo.push(Step::Category {
                    address: s.clone(),
                    before,
                    after: Some(category),
                });
            }
            Ok(undo)
        },
    )?;
    let summary = format!(
        "filed {} under {}",
        unique.join(", "),
        super::mail::category(category)
    );
    Ok(Reply {
        targets: unique.clone(),
        summary: summary.clone(),
        undo,
        effect: Some(Effect::Changed {
            summary: summary.clone(),
        }),
        ..Reply::new(
            json!({ "senders": unique, "category": super::mail::category(category), "note": "Their mail, past and future, in every account." }),
            format!(
                "{}, in every account, now and as it arrives.",
                capitalize(&summary)
            ),
        )
    })
}

/// Every account's id: sender settings count for all of them.
fn all_accounts(env: &Env) -> Result<Vec<i64>, Fail> {
    Ok(env
        .store()
        .accounts()
        .map_err(db)?
        .into_iter()
        .map(|a| a.id)
        .collect())
}

/// Spark's follow-up reminder: the email comes back to the top of the Inbox
/// at `date` unless someone answered. spark-rs sets them on email you sent.
fn remind(env: &Env, a: &Args, view: &View, named: &[&Message], set: bool) -> Done {
    let at = if set {
        let date = a
            .str("date")
            .ok_or("Pass date: when to be reminded, e.g. 2026-04-15.")?;
        let at = parse_time(date, false)?;
        if at <= now_ms() {
            return Err(Fail::Error(format!(
                "{date} has passed; pick a time to come."
            )));
        }
        Some(at)
    } else {
        None
    };
    let own: HashSet<String> = view
        .accounts
        .iter()
        .map(|a| a.email.to_lowercase())
        .collect();
    if set
        && let Some(m) = named
            .iter()
            .find(|m| !own.contains(&m.from_address.to_lowercase()))
    {
        return Err(Fail::Error(format!(
            "Email {} isn't one the user sent. spark-rs reminds about email you sent if nobody answers; snooze this one instead.",
            m.id
        )));
    }
    let accounts: Vec<i64> = named.iter().map(|m| m.account_id).collect();
    let undo = env.guarded(&accounts, Access::Triage, "set reminders", |store| {
        let mut undo = Vec::new();
        for m in named {
            match at {
                Some(at) => {
                    let header = store.headers(m.id).map_err(db)?.and_then(|h| h.message_id);
                    let id = store
                        .add_reminder(&NewReminder {
                            account_id: m.account_id,
                            message_id: header.as_deref().map(|h| h.trim_matches(['<', '>'])),
                            reply_to: None,
                            subject: &m.subject,
                            to: &m.to.join(", "),
                            sent_at: m.date,
                            remind_at: at,
                            if_no_reply: true,
                        })
                        .map_err(db)?;
                    undo.push(Step::Reminder { id });
                }
                None => {
                    for id in store.reminders_on(m.id).map_err(db)? {
                        store.set_reminder(id, None).map_err(db)?;
                    }
                }
            }
        }
        Ok(undo)
    })?;
    let summary = match at {
        Some(at) => format!(
            "set a reminder on {} for {}",
            super::mail::emails_word(named.len()),
            when(at)
        ),
        None => format!(
            "cleared the reminders on {}",
            super::mail::emails_word(named.len())
        ),
    };
    Ok(Reply {
        targets: named.iter().map(|m| m.id.to_string()).collect(),
        summary: summary.clone(),
        undo,
        effect: Some(Effect::Changed {
            summary: summary.clone(),
        }),
        ..Reply::new(
            json!({ "changed": named.iter().map(|m| m.id).collect::<Vec<_>>(), "remind_at": at.map(iso) }),
            format!("{}.", capitalize(&summary)),
        )
    })
}

async fn unsubscribe(env: &Env<'_>, ids: &[String]) -> Done {
    let mut results = Vec::new();
    let mut lines = Vec::new();
    for id in ids {
        let id = message_id(id)?;
        let (m, account) = env.message(id)?;
        env.need(&account, Access::Triage, "unsubscribe")?;
        let result = env
            .engine
            .unsubscribe(id)
            .await
            .map_err(|e| Fail::Error(format!("Email {id}: {e}")))?;
        match result {
            Unsubscribed::Done => {
                lines.push(format!("Email {id}: unsubscribed from {}.", m.from_address));
                results.push(json!({ "id": id, "result": "unsubscribed" }));
            }
            Unsubscribed::Email { to, subject, body } => {
                // The list asks for an email, which goes from the account.
                let draft = spark_mail::draft::Draft {
                    account: m.account_id,
                    to: to.clone(),
                    subject,
                    body,
                    send_at: now_ms(),
                    ..Default::default()
                };
                let json = serde_json::to_string(&draft).map_err(|e| Fail::Error(e.to_string()))?;
                env.guarded(&[m.account_id], Access::Triage, "unsubscribe", |store| {
                    store
                        .queue_send(m.account_id, &json, draft.send_at, &[])
                        .map_err(db)
                })?;
                (env.nudge)(m.account_id);
                lines.push(format!(
                    "Email {id}: sent the list's unsubscribe email to {to}."
                ));
                results.push(json!({ "id": id, "result": "emailed", "to": to }));
            }
            Unsubscribed::Page { url } => {
                lines.push(format!(
                    "Email {id}: the list only unsubscribes on its web page: {url}"
                ));
                results.push(json!({ "id": id, "result": "page", "url": url }));
            }
        }
    }
    Ok(Reply {
        targets: ids.to_vec(),
        summary: format!("unsubscribed from {}", count(ids.len(), "list", "lists")),
        ..Reply::new(json!({ "results": results }), lines.join("\n"))
    })
}

/// A queued send an id names: `s12` for outbox 12, or the saved draft a
/// send was queued from.
fn waiting(env: &Env, id: &str) -> Result<Option<spark_mail::engine::WaitingSend>, Fail> {
    let sends = super::outbox(env)?;
    let id = id.trim();
    if let Some(outbox) = id.strip_prefix('s').and_then(|n| n.parse::<i64>().ok()) {
        return sends
            .into_iter()
            .find(|w| w.id == outbox)
            .map(Some)
            .ok_or_else(|| {
                Fail::Error(format!(
                    "Nothing scheduled is called {id}. emails Scheduled lists it."
                ))
            });
    }
    let draft = message_id(id)?;
    Ok(sends.into_iter().find(|w| w.draft.draft_id == Some(draft)))
}

/// `action send`: sends saved drafts (after the usual 30 seconds the user
/// can undo in), schedules them with `date`, or moves or sends now what's
/// scheduled.
async fn send(env: &Env<'_>, a: &Args<'_>, ids: &[String]) -> Done {
    let at = match a.str("date") {
        Some(date) => {
            let at = parse_time(date, false)?;
            if at <= now_ms() {
                return Err(Fail::Error(format!(
                    "{date} has passed; schedule it for a time to come."
                )));
            }
            Some(at)
        }
        None => None,
    };
    let mut lines = Vec::new();
    let mut results = Vec::new();
    let mut effect = None;
    for id in ids {
        if let Some(w) = waiting(env, id)? {
            let account = env.account(w.account_id)?;
            env.need(&account, Access::Send, "send mail")?;
            let moved = env
                .engine
                .reschedule(w.id, at)
                .await
                .map_err(|e| Fail::Error(e.to_string()))?;
            if at.is_none() && moved == QueuedChange::Moved {
                (env.nudge)(w.account_id);
            }
            let line = match (moved, at) {
                (QueuedChange::Moved, Some(at)) => {
                    format!("{id} rescheduled to send on {}.", when(at))
                }
                (QueuedChange::Moved, None) => format!("{id} is sending now."),
                (QueuedChange::Sent, _) => format!("{id} was already sent."),
                (QueuedChange::Failed { draft_id }, _) => format!(
                    "{id} couldn't be sent and is back in Drafts{}.",
                    draft_id
                        .map(|d| format!(" as draft {d}"))
                        .unwrap_or_default()
                ),
                (QueuedChange::TakenBack { draft_id }, _) => {
                    format!("{id} is back in Drafts as draft {draft_id}.")
                }
                (QueuedChange::Gone, _) => format!("{id} is no longer waiting to send."),
            };
            results.push(
                json!({ "id": id, "result": serde_json::to_value(moved).unwrap_or_default() }),
            );
            lines.push(line);
            continue;
        }
        let draft_id = message_id(id)?;
        let (m, account) = env.message(draft_id)?;
        if !env.store().is_draft(draft_id).map_err(db)? {
            return Err(Fail::Error(format!(
                "Email {draft_id} isn't a draft. draft writes one; action send sends it."
            )));
        }
        env.need(&account, Access::Send, "send mail")?;
        let mut draft = mailbox::open_live(env.engine, draft_id)
            .await
            .map_err(Fail::Error)?;
        if draft.to.trim().is_empty() && draft.cc.trim().is_empty() && draft.bcc.trim().is_empty() {
            return Err(Fail::Error(format!(
                "Draft {draft_id} has no recipients: draft edit={draft_id} to=… adds them."
            )));
        }
        if draft.subject.trim().is_empty() {
            return Err(Fail::Error(format!(
                "Draft {draft_id} has no subject: draft edit={draft_id} subject=… adds one."
            )));
        }
        for field in [&draft.to, &draft.cc, &draft.bcc] {
            spark_mail::draft::recipients(field)
                .map_err(|e| Fail::Error(format!("Draft {draft_id}: {e}")))?;
        }
        draft
            .check_size()
            .map_err(|e| Fail::Error(format!("Draft {draft_id}: {e}")))?;
        if draft
            .body_html
            .as_deref()
            .is_some_and(|h| h.contains("data-placeholder="))
        {
            return Err(Fail::Error(format!(
                "Draft {draft_id} still has template placeholders to fill. Edit its body, or let the user fill them in spark-rs."
            )));
        }
        draft.draft_id = Some(draft_id);
        draft.send_at = at.unwrap_or_else(|| now_ms() + SEND_DELAY_MS);
        draft.send_later = at.map(|at| SendLater {
            key: "custom".into(),
            at,
        });
        let queued = env.prefs.with_agent_access(|grants| {
            refuse(env.agent, grants, &account, Access::Send, "send mail")?;
            mailbox::queue_live(env.engine, &draft).map_err(Fail::Error)
        })?;
        if let Some(signature) = draft
            .signature_id
            .filter(|_| draft.clean_signature().is_some())
        {
            env.store()
                .use_signature(&account.email, signature, now_ms())
                .map_err(db)?;
        }
        let to = if draft.to.is_empty() {
            draft.cc.clone()
        } else {
            draft.to.clone()
        };
        match at {
            Some(at) => {
                lines.push(format!(
                    "Draft {draft_id} scheduled to send on {}. Scheduled ID: s{queued}.",
                    when(at)
                ));
                effect = Some(Effect::Changed {
                    summary: format!("scheduled an email to {to}"),
                });
            }
            None => {
                lines.push(format!(
                    "Draft {draft_id} sends in {} seconds unless the user undoes it in spark-rs. Scheduled ID: s{queued}.",
                    SEND_DELAY_MS / 1000
                ));
                effect = Some(Effect::Sending {
                    id: queued,
                    to: to.clone(),
                    subject: draft.subject.clone(),
                    send_at: draft.send_at,
                });
            }
        }
        results.push(json!({
            "id": draft_id,
            "scheduled_id": format!("s{queued}"),
            "account": account.email,
            "to": to,
            "subject": m.subject,
            "sends_at": iso(draft.send_at),
        }));
    }
    Ok(Reply {
        targets: ids.to_vec(),
        summary: match at {
            Some(at) => format!(
                "scheduled {} for {}",
                count(ids.len(), "email", "emails"),
                when(at)
            ),
            None => format!("sent {}", count(ids.len(), "email", "emails")),
        },
        effect,
        ..Reply::new(json!({ "results": results }), lines.join("\n"))
    })
}

async fn unschedule(env: &Env<'_>, ids: &[String]) -> Done {
    let mut lines = Vec::new();
    let mut results = Vec::new();
    for id in ids {
        let w = waiting(env, id)?.ok_or_else(|| {
            Fail::Error(format!(
                "{id} isn't scheduled. emails Scheduled lists what is."
            ))
        })?;
        let account = env.account(w.account_id)?;
        env.need(&account, Access::Send, "change scheduled mail")?;
        let change = env
            .engine
            .unschedule(w.id, false)
            .await
            .map_err(|e| Fail::Error(e.to_string()))?;
        lines.push(match &change {
            QueuedChange::TakenBack { draft_id } => {
                format!("{id} unscheduled: it's back in Drafts as draft {draft_id}.")
            }
            QueuedChange::Sent => format!("{id} was already sent."),
            QueuedChange::Failed { .. } => format!("{id} had failed and is back in Drafts."),
            QueuedChange::Moved => format!("{id} is still scheduled."),
            QueuedChange::Gone => format!("{id} is no longer waiting to send."),
        });
        results
            .push(json!({ "id": id, "result": serde_json::to_value(change).unwrap_or_default() }));
    }
    Ok(Reply {
        targets: ids.to_vec(),
        summary: format!("unscheduled {}", count(ids.len(), "email", "emails")),
        effect: Some(Effect::Changed {
            summary: "unscheduled an email".into(),
        }),
        ..Reply::new(json!({ "results": results }), lines.join("\n"))
    })
}

// ---- Contacts ----

pub fn contacts(env: &Env, a: &Args) -> Done {
    env.readable()?;
    let limit = a.int("limit")?.unwrap_or(25);
    if !(1..=1000).contains(&limit) {
        return Err("limit must be from 1 to 1000.".into());
    }
    let limit = limit as usize;
    let now = now_ms();
    let store = env.store();
    let (query, kind, found) = match a.str("query") {
        Some(q) => {
            let strict = store.suggest_contacts(q, now, limit).map_err(db)?;
            if strict.is_empty() {
                // Then anything with it inside the name or address.
                let wanted = q.to_lowercase();
                let fuzzy: Vec<_> = store
                    .top_contacts(now, 5_000)
                    .map_err(db)?
                    .into_iter()
                    .filter(|c| {
                        c.name.to_lowercase().contains(&wanted)
                            || c.address.to_lowercase().contains(&wanted)
                    })
                    .take(limit)
                    .collect();
                (Some(q), "fuzzy", fuzzy)
            } else {
                (Some(q), "strict", strict)
            }
        }
        None => (None, "top", store.top_contacts(now, limit).map_err(db)?),
    };
    let list: Vec<Value> = found
        .iter()
        .map(|c| json!({ "name": c.name, "email": c.address }))
        .collect();
    let value = json!({ "query": query, "match": kind, "contacts": list });
    let mut out = String::new();
    if list.is_empty() {
        out.push_str("No contacts found.");
    }
    for c in &found {
        if c.name.is_empty() {
            out.push_str(&format!("{}\n", c.address));
        } else {
            out.push_str(&format!("{} <{}>\n", c.name, c.address));
        }
    }
    let summary = match query {
        Some(q) => format!("looked up “{q}” in your contacts"),
        None => "browsed your contacts".to_owned(),
    };
    Ok(Reply::new(value, out.trim_end().to_owned()).said(summary))
}

pub fn contact(env: &Env, a: &Args) -> Done {
    env.readable()?;
    let email = a
        .str("email")
        .ok_or("Pass email: the contact's address.")?
        .to_lowercase();
    if domain_of(&email).is_none() {
        return Err(Fail::Error(format!("{email} isn't an email address.")));
    }
    let store = env.store();
    let now = now_ms();
    let known = store
        .suggest_contacts(&email, now, 50)
        .map_err(db)?
        .into_iter()
        .find(|c| c.address.eq_ignore_ascii_case(&email));
    let wrote = store
        .messages()
        .map_err(db)?
        .iter()
        .any(|m| m.from_address.eq_ignore_ascii_case(&email));
    if known.is_none() && !wrote {
        return Err(Fail::Error(format!(
            "spark-rs doesn't know {email}: no mail from or to them."
        )));
    }
    let category = store.sender_category(&email).map_err(db)?;
    let priority = store
        .priority_senders()
        .map_err(db)?
        .iter()
        .any(|s| s.eq_ignore_ascii_case(&email));
    let status = store.sender_status(&email).map_err(db)?;
    let decision = |d: Option<Decision>| match d {
        Some(Decision::Accepted) => "accepted",
        Some(Decision::Blocked) => "blocked",
        None => "new",
    };
    let value = json!({
        "email": email,
        "name": known.map(|c| c.name).unwrap_or_default(),
        "category": category.map_or("automatic", super::mail::category),
        "is_priority": priority,
        "blocked": status.sender == Some(Decision::Blocked),
        "accepted": status.sender == Some(Decision::Accepted),
        "gatekeeper": decision(status.sender),
        "domain": decision(status.domain),
    });
    Ok(Reply::new(value.clone(), text::generic(&value)).said(format!("looked up {email}")))
}

pub fn contact_action(env: &Env, a: &Args) -> Done {
    let verb = a
        .str("action_name")
        .or_else(|| a.str("action"))
        .ok_or("Pass action_name, e.g. blockContact or changeCategoryPersonal.")?
        .to_owned();
    let emails = a.addresses("emails");
    if emails.is_empty() {
        return Err("Pass emails: the contacts' addresses.".into());
    }
    env.readable()?;
    match verb.as_str() {
        "changeCategoryPersonal" | "changeCategoryNotification" | "changeCategoryNewsletters" => {
            categorize(env, &verb, &emails)
        }
        "markContactAsPrimary" | "unmarkContactAsPrimary" => prioritize(env, &emails, verb == "markContactAsPrimary"),
        "acceptContact" | "blockContact" | "acceptDomain" | "blockDomain" => decide(env, &verb, &emails),
        "groupEmailsFromContact" | "groupEmailsFromContactAndShowInInbox" | "ungroupEmailsFromContact" => Err(
            "spark-rs groups a sender's mail from its Inbox (right-click → Group), not from agents yet.".into(),
        ),
        "markContactAsImportant" | "unmarkContactAsImportant" => Err(
            "spark-rs doesn't have per-contact notifications yet. Priority senders (markContactAsPrimary) come closest.".into(),
        ),
        "enableAutosummaryForContact" | "disableAutosummaryForContact" => {
            Err("spark-rs doesn't summarize senders' mail automatically yet.".into())
        }
        other => Err(Fail::Error(format!(
            "spark-rs doesn't know the contact action {other}. They are: changeCategoryPersonal, changeCategoryNotification, changeCategoryNewsletters, markContactAsPrimary, unmarkContactAsPrimary, acceptContact, blockContact, acceptDomain, blockDomain."
        ))),
    }
}

fn prioritize(env: &Env, emails: &[String], on: bool) -> Done {
    let all = all_accounts(env)?;
    let addresses: Vec<String> = emails.iter().map(|e| e.trim().to_lowercase()).collect();
    if let Some(bad) = addresses.iter().find(|a| domain_of(a).is_none()) {
        return Err(Fail::Error(format!("{bad} isn't an email address.")));
    }
    let undo = env.guarded(&all, Access::Triage, "change Priority senders", |store| {
        store.prioritize_senders(&addresses, on).map_err(db)
    })?;
    let summary = format!(
        "{} {}",
        if on {
            "made Priority:"
        } else {
            "no longer Priority:"
        },
        addresses.join(", ")
    );
    Ok(Reply {
        targets: addresses.clone(),
        summary: summary.clone(),
        undo: vec![Step::Priority { undo }],
        effect: Some(Effect::Changed {
            summary: summary.clone(),
        }),
        ..Reply::new(
            json!({ "senders": addresses, "priority": on }),
            format!("{}.", capitalize(&summary)),
        )
    })
}

fn decide(env: &Env, verb: &str, emails: &[String]) -> Done {
    let decision = if verb.starts_with("accept") {
        Decision::Accepted
    } else {
        Decision::Blocked
    };
    let by_domain = verb.ends_with("Domain");
    let mut changes: Vec<Change> = Vec::new();
    for e in emails {
        let screen = if by_domain {
            // A domain, or an address at it.
            let domain =
                domain_of(e).unwrap_or_else(|| e.trim().trim_start_matches('@').to_lowercase());
            if !domain.contains('.') {
                return Err(Fail::Error(format!("{e} isn't a domain or an address.")));
            }
            Screen::Domain(domain)
        } else {
            if domain_of(e).is_none() {
                return Err(Fail::Error(format!("{e} isn't an email address.")));
            }
            Screen::Sender(e.trim().to_owned())
        };
        if !changes.iter().any(|c| c.screen == screen) {
            changes.push(Change {
                screen,
                decision: Some(decision),
            });
        }
    }
    let all = all_accounts(env)?;
    let undo = env.guarded(&all, Access::Triage, "accept or block senders", |store| {
        store
            .decide(&changes, now_ms())
            .map_err(|e| Fail::Error(e.to_string()))
    })?;
    let names: Vec<String> = changes
        .iter()
        .map(|c| match &c.screen {
            Screen::Sender(s) => s.clone(),
            Screen::Domain(d) => format!("everyone at {d}"),
        })
        .collect();
    let summary = format!(
        "{} {}",
        if decision == Decision::Accepted {
            "accepted"
        } else {
            "blocked"
        },
        names.join(", ")
    );
    Ok(Reply {
        targets: names.clone(),
        summary: summary.clone(),
        undo: vec![Step::Gatekeeper { undo }],
        effect: Some(Effect::Changed {
            summary: summary.clone(),
        }),
        ..Reply::new(
            json!({ "decided": names, "note": "Their mail follows in every account; moves reach the server after a few seconds." }),
            format!(
                "{}. Their mail follows in every account.",
                capitalize(&summary)
            ),
        )
    })
}
