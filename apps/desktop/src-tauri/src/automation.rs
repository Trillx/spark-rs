//! Inbox automation: Spark's Auto-Labels and Auto-Drafts, run on this
//! computer with the user's own AI provider. A worker looks at new inbox
//! mail a few emails at a time: AI gives each one of Spark's labels and says
//! whether it needs a reply, and for those that do, it writes a reply that
//! waits in Drafts. The store keeps what was decided (`spark_store`'s
//! `automation`); the sync rounds put labels and drafts on the server.

mod prompts;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use spark_ai::{AiClient, ChatRequest, StopReason};
use spark_mail::draft::Draft;
use spark_mail::{Engine, signature};
use spark_store::{
    AccountKind, AutoLabel, AutomationSettings, Candidate, Category, DRAFT_GRACE_MS,
    DraftInstruction, DraftOption, MessageState, Store, now_ms,
};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Notify;

use crate::ai::{AiState, describe, redact};
use crate::mailbox::{Mailbox, text};
use crate::thread_window::Editing;
use prompts::{Email, User, Writing};

/// Emails sorted with one request.
const BATCH: usize = 8;
/// Drafts written in a round at most, so sorting the rest isn't held up.
const DRAFTS_PER_ROUND: usize = 3;
/// How often the worker looks without being woken, and after AI failed.
const LOOK_EVERY: Duration = Duration::from_secs(60);
const AFTER_FAILING: Duration = Duration::from_secs(5 * 60);
/// How often untouched auto-drafts are cleaned up.
const CLEANUP_EVERY: Duration = Duration::from_secs(60 * 60);
/// Earlier emails of a conversation a reply reads first.
const EARLIER: usize = 4;

const NOTHING_TO_DRAFT: &str = "There was nothing in this email to reply to.";

pub struct Automation {
    wake: Notify,
    /// Why the last round couldn't finish, for Settings; `None` once one does.
    problem: Mutex<Option<String>>,
    /// Emails a reply is being written for, so one isn't written twice.
    writing: Mutex<HashSet<i64>>,
}

impl Automation {
    pub fn new() -> Self {
        Self {
            wake: Notify::new(),
            problem: Mutex::new(None),
            writing: Mutex::new(HashSet::new()),
        }
    }

    fn problem(&self, problem: Option<String>) {
        *self.problem.lock().unwrap() = problem;
    }
}

/// Starts the worker.
pub fn start(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut cleaned: Option<std::time::Instant> = None;
        loop {
            let wait = round(&app).await;
            if cleaned.is_none_or(|at| at.elapsed() >= CLEANUP_EVERY) {
                cleanup(&app).await;
                cleaned = Some(std::time::Instant::now());
            }
            // After the provider failed, new mail doesn't cut the wait short.
            let held = wait == AFTER_FAILING;
            let until = tokio::time::Instant::now() + wait;
            let automation = app.state::<Automation>();
            loop {
                tokio::select! {
                    _ = automation.wake.notified() => if !held { break },
                    _ = tokio::time::sleep_until(until) => break,
                }
            }
        }
    });
}

/// New mail came in, or the settings changed: the worker looks now.
pub fn wake(app: &AppHandle) {
    app.state::<Automation>().wake.notify_one();
}

/// Looks at the mail waiting for it, and writes the replies due, until
/// there's nothing left. Returns how long to wait before looking again.
async fn round(app: &AppHandle) -> Duration {
    let mailbox = app.state::<Mailbox>();
    if !mailbox.live() {
        return LOOK_EVERY;
    }
    let engine = mailbox.live.engine.clone();
    let automation = app.state::<Automation>();
    // What the last batch was: one that comes back the same wasn't recorded,
    // and isn't sent to AI again until the next round. Replies tried this
    // round aren't tried again in it either.
    let mut last: Vec<i64> = Vec::new();
    let mut tried: HashSet<i64> = HashSet::new();
    loop {
        let Some(settings) = saved_settings(&engine) else {
            return LOOK_EVERY;
        };
        if !settings.is_on() {
            automation.problem(None);
            return LOOK_EVERY;
        }
        let (client, key) = match app
            .state::<AiState>()
            .background_client(settings.model.as_deref())
        {
            Ok(ready) => ready,
            Err(message) => {
                automation.problem(Some(message));
                return LOOK_EVERY;
            }
        };
        let mut worked = false;

        let batch = match next_batch(&engine.store(), &settings, &automation) {
            Ok(batch) => batch.filter(|b| !b.ids.is_empty() && b.ids != last),
            Err(e) => {
                eprintln!("spark-rs: inbox automation: {e}");
                return LOOK_EVERY;
            }
        };
        if let Some(batch) = batch {
            worked = true;
            last = batch.ids.clone();
            let answer = complete(&client, &batch.request).await;
            let now = now_ms();
            let verdicts = match answer {
                Ok(Some(text)) => {
                    prompts::read_verdicts(&text, batch.ids.len()).unwrap_or_else(|e| {
                        eprintln!("spark-rs: inbox automation: {e}");
                        vec![None; batch.ids.len()]
                    })
                }
                // Declined: these emails are tried again later, the rest go on.
                Ok(None) => vec![None; batch.ids.len()],
                Err(e) => {
                    // The provider can't be reached or refuses the key: nothing
                    // is held against the emails, which wait for it.
                    automation.problem(Some(redact(describe(&e), key.as_deref())));
                    return AFTER_FAILING;
                }
            };
            // What the user wants now, not when the request went out.
            let Some(settings) = saved_settings(&engine) else {
                return LOOK_EVERY;
            };
            let push = push_labels(&engine, batch.account);
            let mut changed = false;
            {
                let mut store = engine.store();
                for (&id, verdict) in batch.ids.iter().zip(verdicts) {
                    let recorded = match verdict {
                        Some(verdict) => store
                            .record_verdict(id, verdict, &settings, push, now)
                            .map(|c| changed |= c),
                        None => store.record_check_failed(id, now),
                    };
                    if let Err(e) = recorded {
                        eprintln!("spark-rs: inbox automation: {e}");
                    }
                }
            }
            if changed {
                let _ = app.emit("mailbox-changed", ());
                mailbox.live.nudge(batch.account);
            }
        }

        let due = engine
            .store()
            .draft_candidates(&settings, now_ms(), DRAFTS_PER_ROUND)
            .unwrap_or_default();
        for id in due {
            if !tried.insert(id) {
                continue;
            }
            worked = true;
            // Asked again before each request: the user may have turned
            // Auto-Drafts off, or answered, meanwhile.
            let Some(settings) = saved_settings(&engine) else {
                return LOOK_EVERY;
            };
            match wanted(&engine.store(), &settings, id) {
                Ok(Wanted::Yes) => {}
                Ok(Wanted::Later) | Err(_) => continue,
                Ok(Wanted::No) => {
                    let _ = engine.store().draft_settled(id);
                    continue;
                }
            }
            // Being written on request already.
            if !automation.writing.lock().unwrap().insert(id) {
                continue;
            }
            let result =
                write_draft(app, &engine, &client, key.as_deref(), id, Asked::Worker).await;
            automation.writing.lock().unwrap().remove(&id);
            let store = engine.store();
            let settled = match result {
                Ok(_) | Err(Failed::Nothing | Failed::Unwanted) => store.draft_settled(id),
                // Still due, for when its sender is accepted.
                Err(Failed::Later) => Ok(()),
                Err(Failed::Email(message)) => {
                    eprintln!("spark-rs: inbox automation: no draft for email {id}: {message}");
                    store.draft_failed(id, now_ms())
                }
                Err(Failed::Provider(message)) => {
                    automation.problem(Some(message));
                    return AFTER_FAILING;
                }
            };
            if let Err(e) = settled {
                eprintln!("spark-rs: inbox automation: {e}");
            }
        }

        if !worked {
            automation.problem(None);
            return LOOK_EVERY;
        }
    }
}

/// The settings as saved now; `None` (and a log line) if they can't be read.
fn saved_settings(engine: &Engine) -> Option<AutomationSettings> {
    engine
        .store()
        .automation_settings()
        .inspect_err(|e| eprintln!("spark-rs: inbox automation: {e}"))
        .ok()
}

/// The next emails to sort, all of one account, and the request for them.
struct Batch {
    account: i64,
    ids: Vec<i64>,
    request: ChatRequest,
}

fn next_batch(
    store: &Store,
    settings: &AutomationSettings,
    automation: &Automation,
) -> spark_store::Result<Option<Batch>> {
    let candidates = store.automation_candidates(settings, now_ms(), BATCH)?;
    let Some(first) = candidates.first() else {
        return Ok(None);
    };
    let account = first.account_id;
    let writing = automation.writing.lock().unwrap().clone();
    let ids: Vec<i64> = candidates
        .iter()
        .filter(|c: &&Candidate| c.account_id == account && !writing.contains(&c.id))
        .map(|c| c.id)
        .collect();
    let Some(user) = user(store, account)? else {
        return Ok(None);
    };
    let mut emails = Vec::new();
    for &id in &ids {
        emails.push(email(store, id)?.unwrap_or_default());
    }
    Ok(Some(Batch {
        account,
        request: prompts::sort_request(&user, &emails),
        ids,
    }))
}

/// Whether a reply to an email is wanted now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wanted {
    Yes,
    /// Not yet: its sender waits in the Gatekeeper, and the settings don't
    /// draft for them until they're accepted.
    Later,
    No,
}

/// Whether email `id` gets a reply drafted: it needs one, its account has
/// Auto-Drafts on since before it came, and nothing answers it already.
fn wanted(store: &Store, settings: &AutomationSettings, id: i64) -> spark_store::Result<Wanted> {
    let Some(m) = store.message(id)? else {
        return Ok(Wanted::No);
    };
    let Some(&since) = settings.draft_accounts.get(&m.account_id) else {
        return Ok(Wanted::No);
    };
    if m.state != MessageState::Inbox
        || m.date < since - DRAFT_GRACE_MS
        || store.is_auto_draft_excluded(&m.from_address)?
        || store.conversation_has_draft(id)?
        || store.answered_after(id)?
    {
        return Ok(Wanted::No);
    }
    Ok(if m.screening && !settings.gatekeeper_drafts {
        Wanted::Later
    } else {
        Wanted::Yes
    })
}

/// Who wants a reply written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asked {
    /// The worker, for mail that needs one: only while it's still wanted.
    Worker,
    /// The user, with "Auto-Draft a reply": whatever the settings say.
    User,
}

/// Why a draft wasn't written.
#[derive(Debug)]
enum Failed {
    /// The provider can't be used now: nothing else can be written either.
    Provider(String),
    /// Something about this email; it may work another time.
    Email(String),
    /// AI found nothing in the email to reply to.
    Nothing,
    /// By the time it was written, it wasn't wanted: the user answered or
    /// drafted a reply, or turned Auto-Drafts off.
    Unwanted,
    /// By the time it was written, its sender was waiting in the Gatekeeper.
    Later,
}

/// Writes the user's reply to email `id`, saves it in Drafts and keeps its
/// options. Returns the draft's id.
async fn write_draft(
    app: &AppHandle,
    engine: &Arc<Engine>,
    client: &AiClient,
    key: Option<&str>,
    id: i64,
    asked: Asked,
) -> Result<i64, Failed> {
    let email_failed = |e: spark_store::Error| Failed::Email(e.to_string());
    let (request, account_id) = {
        let store = engine.store();
        let message = store
            .message(id)
            .map_err(email_failed)?
            .ok_or_else(|| Failed::Email("That email is gone.".into()))?;
        let account = message.account_id;
        let user = user(&store, account)
            .map_err(email_failed)?
            .ok_or_else(|| Failed::Email("Its account is gone.".into()))?;
        let mut conversation = Vec::new();
        for earlier in store
            .conversation_before(id, EARLIER)
            .map_err(email_failed)?
        {
            if let Some(e) = email(&store, earlier).map_err(email_failed)? {
                conversation.push(e);
            }
        }
        conversation.push(
            email(&store, id)
                .map_err(email_failed)?
                .ok_or_else(|| Failed::Email("That email is gone.".into()))?,
        );
        let instructions: Vec<String> = store
            .draft_instructions()
            .map_err(email_failed)?
            .into_iter()
            .filter(|i: &DraftInstruction| i.account_id.is_none_or(|a| a == account))
            .map(|i| i.text)
            .collect();
        let signed = signature::for_reply(&store, &user.address, id)
            .map_err(email_failed)?
            .is_some();
        let writing = Writing {
            user: &user,
            instructions: &instructions,
            signed,
        };
        (prompts::reply_request(&writing, &conversation), account)
    };
    let answer = complete(client, &request)
        .await
        .map_err(|e| Failed::Provider(redact(describe(&e), key)))?
        .ok_or_else(|| Failed::Email("The AI provider declined to write it.".into()))?;
    let options = prompts::read_options(&answer).map_err(Failed::Email)?;
    if options.is_empty() {
        return Err(Failed::Nothing);
    }
    let draft_id = save_draft(engine, id, &options, asked)?;
    let _ = app.emit("mailbox-changed", ());
    app.state::<Mailbox>().live.nudge(account_id);
    Ok(draft_id)
}

/// Saves `options`' first as a reply to email `id`, the way the composer
/// starts one: to Reply-To or the sender, with the signature a reply gets.
/// For the worker, only if it's still wanted, checked as it's saved.
fn save_draft(
    engine: &Engine,
    id: i64,
    options: &[DraftOption],
    asked: Asked,
) -> Result<i64, Failed> {
    let failed = |e: spark_store::Error| Failed::Email(e.to_string());
    let mut store = engine.store();
    if asked == Asked::Worker {
        let settings = store.automation_settings().map_err(failed)?;
        match wanted(&store, &settings, id).map_err(failed)? {
            Wanted::Yes => {}
            Wanted::Later => return Err(Failed::Later),
            Wanted::No => return Err(Failed::Unwanted),
        }
    }
    let message = store
        .message(id)
        .map_err(failed)?
        .ok_or_else(|| Failed::Email("That email is gone.".into()))?;
    let account = store
        .account(message.account_id)
        .map_err(failed)?
        .ok_or_else(|| Failed::Email("Its account is gone.".into()))?;
    let signature = signature::for_reply(&store, &account.email, id).map_err(failed)?;
    let subject = message.subject.trim();
    let draft = Draft {
        account: account.id,
        to: if message.reply_to.is_empty() {
            entry(&message.from_name, &message.from_address)
        } else {
            message.reply_to.join(", ")
        },
        subject: if subject.to_lowercase().starts_with("re:") {
            subject.to_owned()
        } else {
            format!("Re: {subject}")
        },
        body: options[0].body.clone(),
        reply_to: Some(id),
        signature: signature.as_ref().map(|s| s.html.clone()),
        signature_id: signature.as_ref().map(|s| s.id),
        ..Default::default()
    };
    let now = now_ms();
    let draft_id = spark_mail::drafts::save(&mut store, &account, &draft, &[], now)
        .map_err(|e| Failed::Email(e.to_string()))?;
    store
        .add_auto_draft(draft_id, id, options, 0, now)
        .map_err(failed)?;
    Ok(draft_id)
}

/// An address as a To entry, its name quoted if it needs to be.
fn entry(name: &str, address: &str) -> String {
    let name = name.trim();
    if name.is_empty() || name.eq_ignore_ascii_case(address) {
        address.to_owned()
    } else if name.contains([',', ';', '<', '>', '"', '@', '(', ')', ':']) {
        format!(
            "\"{}\" <{address}>",
            name.replace('\\', "\\\\").replace('"', "\\\"")
        )
    } else {
        format!("{name} <{address}>")
    }
}

/// The answer's text; `None` when the provider declined or said nothing.
async fn complete(
    client: &AiClient,
    request: &ChatRequest,
) -> Result<Option<String>, spark_ai::AiError> {
    let completion = client.complete(request, |_| {}).await?;
    Ok(match completion.stop_reason {
        StopReason::Refusal { .. } => None,
        _ if completion.text.trim().is_empty() => None,
        _ => Some(completion.text),
    })
}

/// Gmail and Microsoft accounts keep labels on the server too.
fn push_labels(engine: &Engine, account_id: i64) -> bool {
    matches!(
        engine.store().account(account_id),
        Ok(Some(a)) if a.kind != AccountKind::Imap
    )
}

fn user(store: &Store, account_id: i64) -> spark_store::Result<Option<User>> {
    Ok(store.account(account_id)?.map(|a| User {
        name: Some(a.name.trim().to_owned()).filter(|n| !n.is_empty()),
        address: a.email,
    }))
}

fn email(store: &Store, id: i64) -> spark_store::Result<Option<Email>> {
    let Some(m) = store.message(id)? else {
        return Ok(None);
    };
    let content = store.content(id)?;
    let (to, cc, text) = match content {
        Some(c) => (c.to, c.cc, c.text),
        None => (m.to.clone(), Vec::new(), m.snippet.clone()),
    };
    Ok(Some(Email {
        from: m.from_name,
        address: m.from_address,
        to,
        cc,
        date: m.date,
        subject: m.subject,
        kind: match m.category {
            Category::People => None,
            Category::Notifications => Some("automated notification, going by its headers"),
            Category::Newsletters => Some("newsletter or bulk mail, going by its headers"),
        },
        text: if text.trim().is_empty() {
            m.snippet
        } else {
            text
        },
    }))
}

/// Deletes auto-drafts the user hasn't touched for the days Settings says:
/// not here, and not on the server (Outlook edits a Microsoft draft in
/// place, so the server is asked first).
async fn cleanup(app: &AppHandle) {
    let mailbox = app.state::<Mailbox>();
    if !mailbox.live() {
        return;
    }
    let engine = mailbox.live.engine.clone();
    let candidates = {
        let store = engine.store();
        let Some(days) = store
            .automation_settings()
            .ok()
            .and_then(|s| s.cleanup_days)
        else {
            return;
        };
        let before = now_ms() - i64::from(days) * 86_400_000;
        store.untouched_auto_drafts(before).unwrap_or_default()
    };
    let mut removed = false;
    for id in candidates {
        match engine.draft_changed_elsewhere(id).await {
            Ok(false) => {}
            Ok(true) => {
                if let Err(e) = engine.store().auto_draft_edited_elsewhere(id) {
                    eprintln!("spark-rs: inbox automation: {e}");
                }
                continue;
            }
            // Can't tell now (offline, or gone from the server): next time.
            Err(_) => continue,
        }
        // Asked again: it may have been edited or sent while the server was asked.
        let mut store = engine.store();
        if store.draft_reserved(id).unwrap_or(true)
            || !store.auto_draft_untouched(id).unwrap_or(false)
        {
            continue;
        }
        match store.discard_draft(id) {
            Ok(gone) => removed |= gone,
            Err(e) => eprintln!("spark-rs: couldn't clean up auto-draft {id}: {e}"),
        }
    }
    if removed {
        let _ = app.emit("mailbox-changed", ());
    }
}

/// The user replied to email `reply_to`: its conversation's To respond
/// label becomes Actioned, as Spark's labels follow the conversation.
pub fn replied(app: &AppHandle, reply_to: i64) {
    let mailbox = app.state::<Mailbox>();
    if !mailbox.live() {
        return;
    }
    let engine = &mailbox.live.engine;
    let Ok(Some(m)) = engine.store().message(reply_to) else {
        return;
    };
    let (account, conversation) = (m.account_id, m.conversation);
    let settings = engine.store().automation_settings().unwrap_or_default();
    if !settings.labels(account) {
        return;
    }
    let to = settings
        .applies(account, AutoLabel::Actioned)
        .then_some(AutoLabel::Actioned);
    let push = push_labels(engine, account);
    let changed =
        engine
            .store()
            .relabel_conversation(conversation, account, AutoLabel::ToRespond, to, push);
    if changed.is_ok_and(|ids| !ids.is_empty()) {
        let _ = app.emit("mailbox-changed", ());
    }
}

/// Settings → Inbox Automation as the UI sees it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationView {
    label_accounts: Vec<i64>,
    labels_off: Vec<AutoLabel>,
    draft_accounts: Vec<i64>,
    cleanup_days: Option<u32>,
    gatekeeper_drafts: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    excluded: Vec<String>,
    instructions: Vec<DraftInstruction>,
    /// Why it isn't working, if it isn't.
    #[serde(skip_serializing_if = "Option::is_none")]
    problem: Option<String>,
}

/// What the settings page saves; accounts just turned on start from now.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationChange {
    label_accounts: Vec<i64>,
    labels_off: Vec<AutoLabel>,
    draft_accounts: Vec<i64>,
    cleanup_days: Option<u32>,
    gatekeeper_drafts: bool,
    model: Option<String>,
}

fn view(store: &Store, automation: &Automation) -> Result<AutomationView, String> {
    let settings = store.automation_settings().map_err(text)?;
    let problem = settings
        .is_on()
        .then(|| automation.problem.lock().unwrap().clone())
        .flatten();
    Ok(AutomationView {
        label_accounts: settings.label_accounts.keys().copied().collect(),
        labels_off: settings.labels_off,
        draft_accounts: settings.draft_accounts.keys().copied().collect(),
        cleanup_days: settings.cleanup_days,
        gatekeeper_drafts: settings.gatekeeper_drafts,
        model: settings.model,
        excluded: store.auto_draft_excluded().map_err(text)?,
        instructions: store.draft_instructions().map_err(text)?,
        problem,
    })
}

fn live(mailbox: &Mailbox) -> Result<(), String> {
    if mailbox.live() {
        Ok(())
    } else {
        Err("Add an account first: inbox automation works on your own mail.".into())
    }
}

#[tauri::command]
pub fn get_inbox_automation(
    mailbox: State<'_, Mailbox>,
    automation: State<'_, Automation>,
) -> Result<AutomationView, String> {
    view(&mailbox.live.engine.store(), &automation)
}

#[tauri::command]
pub fn save_inbox_automation(
    app: AppHandle,
    mailbox: State<'_, Mailbox>,
    automation: State<'_, Automation>,
    change: AutomationChange,
) -> Result<AutomationView, String> {
    live(&mailbox)?;
    if change.cleanup_days.is_some_and(|d| !(1..=365).contains(&d)) {
        return Err("Choose between 1 and 365 days.".into());
    }
    let view = {
        let mut store = mailbox.live.engine.store();
        let before = store.automation_settings().map_err(text)?;
        let now = now_ms();
        let since = |on: &std::collections::BTreeMap<i64, i64>, ids: &[i64]| {
            ids.iter()
                .map(|id| (*id, on.get(id).copied().unwrap_or(now)))
                .collect()
        };
        let settings = AutomationSettings {
            label_accounts: since(&before.label_accounts, &change.label_accounts),
            labels_off: change.labels_off,
            draft_accounts: since(&before.draft_accounts, &change.draft_accounts),
            cleanup_days: change.cleanup_days,
            gatekeeper_drafts: change.gatekeeper_drafts,
            model: change
                .model
                .map(|m| m.trim().to_owned())
                .filter(|m| !m.is_empty()),
        };
        store.set_automation_settings(&settings).map_err(text)?;
        view(&store, &automation)?
    };
    let _ = app.emit("mailbox-changed", ());
    wake(&app);
    Ok(view)
}

/// Shows `label` on an email instead of its own; `None` takes it off.
#[tauri::command]
pub fn set_auto_label(
    app: AppHandle,
    mailbox: State<'_, Mailbox>,
    id: i64,
    label: Option<AutoLabel>,
) -> Result<(), String> {
    if !mailbox.live() {
        return mailbox.demo.set_auto_label(id, label);
    }
    let engine = &mailbox.live.engine;
    let account = engine
        .store()
        .message(id)
        .map_err(text)?
        .ok_or("That email is gone.")?
        .account_id;
    let push = push_labels(engine, account);
    if engine
        .store()
        .set_auto_label(id, label, push)
        .map_err(text)?
    {
        let _ = app.emit("mailbox-changed", ());
        mailbox.live.nudge(account);
    }
    Ok(())
}

/// Spark's "Auto-Draft a reply": writes a reply to email `id` now, whatever
/// the settings say. Returns the draft's id.
#[tauri::command]
pub async fn auto_draft_reply(
    app: AppHandle,
    mailbox: State<'_, Mailbox>,
    automation: State<'_, Automation>,
    id: i64,
) -> Result<i64, String> {
    live(&mailbox)?;
    let engine = mailbox.live.engine.clone();
    if let Some(draft) = engine.store().auto_draft_for(id).map_err(text)? {
        return Ok(draft.draft_id);
    }
    let model = engine.store().automation_settings().map_err(text)?.model;
    let (client, key) = app.state::<AiState>().background_client(model.as_deref())?;
    if !automation.writing.lock().unwrap().insert(id) {
        return Err("A reply to this email is being written already.".into());
    }
    let _ = app.emit("auto-draft-writing", WritingEvent { id, writing: true });
    let result = write_draft(&app, &engine, &client, key.as_deref(), id, Asked::User).await;
    automation.writing.lock().unwrap().remove(&id);
    let _ = app.emit("auto-draft-writing", WritingEvent { id, writing: false });
    match result {
        Ok(draft) => {
            // Written: the worker has none left to write for it.
            let _ = engine.store().draft_settled(id);
            Ok(draft)
        }
        Err(Failed::Provider(message) | Failed::Email(message)) => Err(message),
        Err(Failed::Nothing | Failed::Unwanted | Failed::Later) => Err(NOTHING_TO_DRAFT.into()),
    }
}

/// `auto-draft-writing`: a reply to email `id` is being written, or done.
#[derive(Clone, Serialize)]
struct WritingEvent {
    id: i64,
    writing: bool,
}

/// The options an auto-draft was written with, for the picker.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoDraftView {
    message_id: i64,
    options: Vec<DraftOption>,
    chosen: usize,
}

#[tauri::command]
pub fn get_auto_draft(
    mailbox: State<'_, Mailbox>,
    id: i64,
) -> Result<Option<AutoDraftView>, String> {
    if !mailbox.live() {
        return Ok(
            crate::demo::auto_draft(id).map(|(message_id, options)| AutoDraftView {
                message_id,
                options,
                chosen: 0,
            }),
        );
    }
    Ok(mailbox
        .live
        .engine
        .store()
        .auto_draft(id)
        .map_err(text)?
        .map(|d| AutoDraftView {
            message_id: d.message_id,
            options: d.options,
            chosen: d.chosen,
        }))
}

/// The auto-draft says option `index` now. Refused once the draft was
/// edited, or while a composer has it open, as that would lose what the
/// user wrote; its files stay as they were.
#[tauri::command]
pub async fn choose_auto_draft_option(
    app: AppHandle,
    mailbox: State<'_, Mailbox>,
    editing: State<'_, Editing>,
    id: i64,
    index: usize,
) -> Result<(), String> {
    live(&mailbox)?;
    if editing.holds(id) {
        return Err("This draft is open in the composer: pick a reply there.".into());
    }
    let engine = mailbox.live.engine.clone();
    let auto = engine
        .store()
        .auto_draft(id)
        .map_err(text)?
        .ok_or("That draft is gone.")?;
    let option = auto
        .options
        .get(index)
        .ok_or("That draft has no such option.")?;
    // Every save changes it: one made while the draft is read below (a
    // composer adding a file, say) means what's read is out of date.
    let read_at = engine.store().draft_saved_at(id).map_err(text)?;
    let mut draft = crate::mailbox::open_live(&engine, id).await?;
    draft.body = option.body.clone();
    draft.body_html = None;
    draft.draft_id = Some(id);
    let account = draft.account;
    let windows = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::mailbox::save_saved_draft_if(
            &engine,
            &draft,
            |store| may_pick(store, id, read_at, windows.state::<Editing>().holds(id)),
            |store, _| store.auto_draft_chosen(id, index).map(|_| ()).map_err(text),
        )
    })
    .await
    .map_err(text)??;
    let _ = app.emit("mailbox-changed", ());
    mailbox.live.nudge(account);
    Ok(())
}

/// Whether auto-draft `id`, read when it was last saved at `read_at`, may
/// say another option now (while the store is held for the save): not while
/// it's sent or open in a composer (`held`), once edited, or if it was saved
/// again since it was read.
fn may_pick(store: &Store, id: i64, read_at: Option<i64>, held: bool) -> Result<(), String> {
    if store.draft_reserved(id).map_err(text)? {
        Err("This draft is being sent.".into())
    } else if held {
        Err("This draft is open in the composer: pick a reply there.".into())
    } else if !store.auto_draft_untouched(id).map_err(text)? {
        Err("This draft was edited, so its other replies are gone.".into())
    } else if store.draft_saved_at(id).map_err(text)? != read_at {
        Err("This draft changed just now. Try again.".into())
    } else {
        Ok(())
    }
}

/// Leaves a sender's mail alone (`excluded`), or no longer does.
#[tauri::command]
pub fn set_auto_draft_excluded(
    mailbox: State<'_, Mailbox>,
    address: String,
    excluded: bool,
) -> Result<Vec<String>, String> {
    live(&mailbox)?;
    let store = mailbox.live.engine.store();
    store
        .set_auto_draft_excluded(&address, excluded)
        .map_err(text)?;
    store.auto_draft_excluded().map_err(text)
}

#[tauri::command]
pub fn add_draft_instruction(
    mailbox: State<'_, Mailbox>,
    text: String,
    account: Option<i64>,
) -> Result<Vec<DraftInstruction>, String> {
    live(&mailbox)?;
    let store = mailbox.live.engine.store();
    store
        .add_draft_instruction(&text, account)
        .map_err(crate::mailbox::text)?;
    store.draft_instructions().map_err(crate::mailbox::text)
}

#[tauri::command]
pub fn remove_draft_instruction(
    mailbox: State<'_, Mailbox>,
    id: i64,
) -> Result<Vec<DraftInstruction>, String> {
    live(&mailbox)?;
    let store = mailbox.live.engine.store();
    store.remove_draft_instruction(id).map_err(text)?;
    store.draft_instructions().map_err(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoSecrets;

    impl spark_mail::Secrets for NoSecrets {
        fn get(&self, _: &str) -> Result<Option<String>, String> {
            Ok(None)
        }
        fn set(&self, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn replies_go_to_a_quoted_name_when_it_needs_quoting() {
        assert_eq!(
            entry("Dana Whitfield", "dana@x.com"),
            "Dana Whitfield <dana@x.com>"
        );
        assert_eq!(
            entry("Whitfield, Dana", "dana@x.com"),
            "\"Whitfield, Dana\" <dana@x.com>"
        );
        assert_eq!(entry("", "dana@x.com"), "dana@x.com");
        assert_eq!(entry("dana@x.com", "dana@x.com"), "dana@x.com");
        for to in [
            entry("Whitfield, Dana", "dana@x.com"),
            entry("A \"B\" C", "c@x.com"),
        ] {
            assert_eq!(spark_mail::draft::recipients(&to).unwrap().len(), 1, "{to}");
        }
    }

    #[test]
    fn the_default_view_is_off() {
        let store = Store::open_in_memory().unwrap();
        let view = view(&store, &Automation::new()).unwrap();
        assert!(view.label_accounts.is_empty() && view.draft_accounts.is_empty());
        assert_eq!(view.cleanup_days, Some(7));
        assert_eq!(view.problem, None);
    }

    /// An engine whose Gmail account has a Drafts folder and one inbox
    /// email from Dana, which it returns too.
    fn one_email() -> (Engine, i64, i64) {
        use spark_store::{Added, FolderRole, Incoming, NewAccount, RemoteFolder};
        let mut store = Store::open_in_memory().unwrap();
        let account = store
            .add_account(&NewAccount {
                kind: AccountKind::Gmail,
                email: "me@example.com",
                name: "Me",
                label: "Personal",
                config: "{}",
            })
            .unwrap();
        store
            .replace_folders(
                account.id,
                &[RemoteFolder {
                    remote_id: "[Gmail]/Drafts".into(),
                    parent: None,
                    name: "Drafts".into(),
                    role: Some(FolderRole::Drafts),
                    selectable: true,
                }],
            )
            .unwrap();
        let Added::New(id) = store
            .add_incoming(
                &Incoming {
                    account_id: account.id,
                    mailbox: Some("INBOX".into()),
                    uid: Some(1),
                    message_id: Some("<1@x.com>".into()),
                    from_name: "Whitfield, Dana".into(),
                    from_address: "dana@x.com".into(),
                    subject: "Lunch".into(),
                    date: now_ms(),
                    ..Default::default()
                },
                false,
            )
            .unwrap()
        else {
            panic!("new");
        };
        let engine = Engine::new(Arc::new(Mutex::new(store)), Arc::new(NoSecrets));
        (engine, account.id, id)
    }

    fn options() -> Vec<DraftOption> {
        vec![
            DraftOption {
                title: "Accept".into(),
                body: "Tuesday works.".into(),
            },
            DraftOption {
                title: "Decline".into(),
                body: "I can't make it.".into(),
            },
        ]
    }

    #[test]
    fn a_reply_draft_is_saved_with_its_options() {
        let (engine, _, id) = one_email();
        let Ok(draft_id) = save_draft(&engine, id, &options(), Asked::User) else {
            panic!("saved");
        };
        let draft = engine.open_draft(draft_id).unwrap().unwrap();
        assert_eq!(draft.to, "\"Whitfield, Dana\" <dana@x.com>");
        assert_eq!(draft.subject, "Re: Lunch");
        assert_eq!(draft.body, "Tuesday works.");
        let store = engine.store();
        assert_eq!(
            store.message(id).unwrap().unwrap().auto_draft,
            Some(draft_id)
        );
        assert_eq!(
            store.auto_draft(draft_id).unwrap().unwrap().options,
            options()
        );
        assert!(store.conversation_has_draft(id).unwrap());
    }

    #[test]
    fn another_option_is_picked_only_over_the_draft_as_it_was_read() {
        let (engine, _, id) = one_email();
        let Ok(draft_id) = save_draft(&engine, id, &options(), Asked::User) else {
            panic!("saved");
        };
        let read_at = engine.store().draft_saved_at(draft_id).unwrap();
        assert!(read_at.is_some());
        assert_eq!(may_pick(&engine.store(), draft_id, read_at, false), Ok(()));
        // A composer has it open.
        assert!(may_pick(&engine.store(), draft_id, read_at, true).is_err());
        // Saved again since it was read (a file added, the text the same).
        std::thread::sleep(std::time::Duration::from_millis(2));
        let mut again = engine.open_draft(draft_id).unwrap().unwrap();
        again.draft_id = Some(draft_id);
        {
            let mut store = engine.store();
            let account = store.account(again.account).unwrap().unwrap();
            spark_mail::drafts::save(&mut store, &account, &again, &[], now_ms()).unwrap();
        }
        assert!(may_pick(&engine.store(), draft_id, read_at, false).is_err());
    }

    #[test]
    fn a_reply_for_a_new_sender_waits_for_them() {
        let (engine, account, known) = one_email();
        let settings = AutomationSettings {
            draft_accounts: [(account, now_ms() - 60_000)].into(),
            ..Default::default()
        };
        engine.store().set_automation_settings(&settings).unwrap();
        assert_eq!(
            wanted(&engine.store(), &settings, known).unwrap(),
            Wanted::Yes
        );
        // Someone new, waiting in the Gatekeeper.
        let added = engine
            .store()
            .add_incoming(
                &spark_store::Incoming {
                    account_id: account,
                    mailbox: Some("INBOX".into()),
                    uid: Some(2),
                    message_id: Some("<2@new.example>".into()),
                    from_address: "someone@new.example".into(),
                    subject: "Hello".into(),
                    date: now_ms(),
                    ..Default::default()
                },
                true,
            )
            .unwrap();
        let spark_store::Added::New(id) = added else {
            panic!("new");
        };
        assert!(engine.store().message(id).unwrap().unwrap().screening);
        assert_eq!(
            wanted(&engine.store(), &settings, id).unwrap(),
            Wanted::Later
        );
        assert!(matches!(
            save_draft(&engine, id, &options(), Asked::Worker),
            Err(Failed::Later)
        ));
        let for_new_senders = AutomationSettings {
            gatekeeper_drafts: true,
            ..settings
        };
        assert_eq!(
            wanted(&engine.store(), &for_new_senders, id).unwrap(),
            Wanted::Yes
        );
    }

    #[test]
    fn the_worker_saves_a_reply_only_while_its_still_wanted() {
        let (engine, account, id) = one_email();
        // Auto-Drafts turned off while AI was writing: dropped.
        assert!(matches!(
            save_draft(&engine, id, &options(), Asked::Worker),
            Err(Failed::Unwanted)
        ));
        let settings = AutomationSettings {
            draft_accounts: [(account, now_ms() - 60_000)].into(),
            ..Default::default()
        };
        engine.store().set_automation_settings(&settings).unwrap();
        // The user wrote a reply of their own meanwhile.
        let own = Draft {
            account,
            to: "dana@x.com".into(),
            subject: "Re: Lunch".into(),
            body: "Let me check.".into(),
            reply_to: Some(id),
            ..Default::default()
        };
        {
            let mut store = engine.store();
            let account = store.account(account).unwrap().unwrap();
            spark_mail::drafts::save(&mut store, &account, &own, &[], now_ms()).unwrap();
        }
        assert!(matches!(
            save_draft(&engine, id, &options(), Asked::Worker),
            Err(Failed::Unwanted)
        ));
        // Asked for by the user, it's written all the same.
        assert!(save_draft(&engine, id, &options(), Asked::User).is_ok());
    }
}
