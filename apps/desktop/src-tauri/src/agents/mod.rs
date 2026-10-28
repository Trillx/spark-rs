//! AI agents on this computer (Claude Code, Codex, …) using the user's mail.
//! Agents run `spark-cli`, which reaches this app over a loopback connection
//! (the protocol is in the `spark-agent` crate). The app runs the commands
//! in `commands/` (Spark's CLI commands) and the older tools in `tools.rs`
//! against the access the user chose per account in Settings → Agents, and
//! tells the UI what agents did.

pub(crate) mod commands;
pub(crate) mod tools;

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use spark_agent::{ENDPOINT_FILE, Endpoint, Request, Response};
use spark_store::now_ms;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::net::TcpListener;

use crate::mailbox::Mailbox;
use crate::notify;
use crate::prefs::PrefsState;
use tools::Effect;

/// What agents may do in an account. Each level includes the ones before.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Access {
    #[default]
    None,
    /// List, search and read mail.
    Read,
    /// Also mark mail Done, read, pinned, set aside, snoozed or deleted.
    Triage,
    /// Also send mail as the user.
    Send,
}

impl Access {
    /// As agents see it in `list_accounts`.
    fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Read => "read",
            Self::Triage => "triage",
            Self::Send => "send",
        }
    }

    /// "read access", for refusals.
    fn label(self) -> String {
        match self {
            Self::None => "no access".into(),
            other => format!("{} access", other.name()),
        }
    }

    /// "for reading", for refusals when nothing is shared.
    fn verb(self) -> &'static str {
        match self {
            Self::None | Self::Read => "reading",
            Self::Triage => "triage",
            Self::Send => "sending",
        }
    }
}

/// What the commands keep while the app runs.
struct AgentState {
    log: commands::Log,
    /// Where `attachment` leaves copies of files for spark-cli.
    files: PathBuf,
}

/// Listens for `spark-cli` and writes where to find this app to
/// `agent-endpoint.json` in the local data folder.
pub fn start(app: &AppHandle) -> Result<(), String> {
    let accounts: Vec<_> = app
        .state::<Mailbox>()
        .live
        .engine
        .store()
        .accounts()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|a| a.id)
        .collect();
    app.state::<PrefsState>().retain_agent_accounts(&accounts)?;
    let dir = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
    let path = dir.join(ENDPOINT_FILE);
    // Copies handed to agents last time aren't needed any more.
    let files = std::env::temp_dir().join("spark-rs").join("agent-files");
    let _ = std::fs::remove_dir_all(&files);
    app.manage(AgentState {
        log: commands::Log::new(dir.join(commands::HISTORY_FILE)),
        files,
    });
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let listening = async {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
            let endpoint = Endpoint::new(listener.local_addr()?.port(), exe)?;
            endpoint.write(&path)?;
            std::io::Result::Ok((listener, endpoint.token))
        };
        match listening.await {
            Ok((listener, token)) => {
                spark_agent::serve(listener, token, move |request| {
                    let app = app.clone();
                    async move { handle(&app, request).await }
                })
                .await
            }
            Err(e) => eprintln!("spark-rs: agents can't connect: {e}"),
        }
    });
    Ok(())
}

/// Removes the endpoint file on quit, so `spark-cli` starts the app again
/// rather than knocking on a closed port.
pub fn stopped(app: &AppHandle) {
    let Ok(dir) = app.path().app_local_data_dir() else {
        return;
    };
    let path = dir.join(ENDPOINT_FILE);
    if Endpoint::read(&path).is_ok_and(|e| e.pid == std::process::id()) {
        let _ = std::fs::remove_file(path);
    }
}

async fn handle(app: &AppHandle, request: Request) -> Response {
    match request.method.as_str() {
        "ping" => Response::Result(json!({
            "app": "spark-rs",
            "version": app.package_info().version.to_string(),
        })),
        "tools/list" => Response::Result(commands::catalog()),
        "tools/call" => {
            let name = request.params["name"].as_str().unwrap_or_default();
            let args = match &request.params["arguments"] {
                Value::Null => json!({}),
                args => args.clone(),
            };
            let agent = agent_name(&request.client);
            if commands::knows(name) {
                let outcome = command(app, &agent, name, &args).await;
                return Response::Result(json!({
                    "content": [{ "type": "text", "text": outcome.text }],
                    "structuredContent": outcome.value,
                    "isError": outcome.is_error,
                }));
            }
            // The older tools, still there for agents set up with them.
            let outcome = run(app, &agent, name, &args);
            Response::Result(json!({
                "content": [{ "type": "text", "text": outcome.text }],
                "isError": outcome.is_error,
            }))
        }
        other => Response::Error(format!("spark-rs doesn't know {other}.")),
    }
}

async fn command(app: &AppHandle, agent: &str, name: &str, args: &Value) -> commands::Outcome {
    let prefs = app.state::<PrefsState>();
    let mailbox = app.state::<Mailbox>();
    let state = app.state::<AgentState>();
    let nudge = |account: i64| mailbox.live.nudge(account);
    let env = commands::Env {
        engine: &mailbox.live.engine,
        prefs: &prefs,
        agent,
        files: &state.files,
        log: &state.log,
        nudge: &nudge,
    };
    let outcome = commands::run(&env, name, args).await;
    tell(app, agent, &outcome.effect);
    outcome
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ActivityEvent {
    text: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SendingEvent {
    id: i64,
    agent: String,
    to: String,
    subject: String,
    send_at: i64,
}

fn run(app: &AppHandle, agent: &str, name: &str, args: &Value) -> tools::Outcome {
    let prefs = app.state::<PrefsState>();
    let mailbox = app.state::<Mailbox>();
    let outcome = prefs.with_agent_access(|access| {
        let mut store = mailbox.live.engine.store();
        let mut cx = tools::Context {
            store: &mut store,
            access,
            agent,
            now: now_ms(),
        };
        tools::call(&mut cx, name, args)
    });
    tell(app, agent, &outcome.effect);
    outcome
}

/// Tells the UI what an agent did.
fn tell(app: &AppHandle, agent: &str, effect: &Option<Effect>) {
    match effect {
        Some(Effect::Changed { summary }) => {
            let _ = app.emit("mailbox-changed", ());
            let _ = app.emit(
                "agent-activity",
                ActivityEvent {
                    text: format!("{agent} {summary}"),
                },
            );
        }
        Some(Effect::Sending {
            id,
            to,
            subject,
            send_at,
        }) => {
            let _ = app.emit(
                "agent-sending",
                SendingEvent {
                    id: *id,
                    agent: agent.to_owned(),
                    to: to.clone(),
                    subject: subject.clone(),
                    send_at: *send_at,
                },
            );
            notify::agent_sending(app, agent, to, subject);
        }
        None => {}
    }
}

/// A readable name from MCP's `clientInfo.name`.
fn agent_name(client: &str) -> String {
    let lower = client.to_lowercase();
    let name = if lower.contains("claude-code") || lower == "claude code" {
        "Claude Code"
    } else if lower.contains("codex") {
        "Codex"
    } else if lower.contains("hermes") {
        "Hermes"
    } else if lower.contains("gemini") {
        "Gemini CLI"
    } else if lower.contains("claude") {
        "Claude"
    } else if lower.is_empty() || lower == "spark-cli" {
        "An agent"
    } else {
        client
    };
    name.to_owned()
}

/// Settings → Agents.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentsView {
    accounts: Vec<AccountAccess>,
    /// `spark-cli` beside the app, which agents run; `None` if it's missing.
    cli: Option<String>,
    /// This computer's name.
    computer: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountAccess {
    id: i64,
    email: String,
    label: String,
    color: String,
    access: Access,
}

fn view(mailbox: &Mailbox, access: &BTreeMap<i64, Access>) -> Result<AgentsView, String> {
    // Demo accounts can't be shared: there's no demo for agents.
    let accounts = mailbox
        .live
        .engine
        .store()
        .accounts()
        .map_err(|e| e.to_string())?;
    let cli = std::env::current_exe()
        .ok()
        .map(|exe| exe.with_file_name(format!("spark-cli{}", std::env::consts::EXE_SUFFIX)))
        .filter(|cli| cli.is_file())
        .map(|cli| cli.display().to_string());
    Ok(AgentsView {
        accounts: accounts
            .into_iter()
            .map(|a| AccountAccess {
                access: access.get(&a.id).copied().unwrap_or_default(),
                id: a.id,
                email: a.email,
                label: a.label,
                color: a.color,
            })
            .collect(),
        cli,
        computer: std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "This computer".into()),
    })
}

/// A row of Settings → Agents → Agent activity.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityRow {
    id: u64,
    at: i64,
    agent: String,
    /// What it did, e.g. "archived “Quote”"; for a failure, what it tried.
    summary: String,
    /// Why it failed or was refused.
    detail: Option<String>,
    /// "success", "error" or "blocked".
    result: String,
    can_undo: bool,
    undone: bool,
}

/// What agents did lately, newest first: what they changed, or with
/// `reads` everything.
#[tauri::command]
pub fn agent_activity(app: AppHandle, reads: bool) -> Vec<ActivityRow> {
    let Some(state) = app.try_state::<AgentState>() else {
        return Vec::new();
    };
    commands::recent(&state.log, reads, 200)
        .into_iter()
        .map(|e| {
            let result = serde_json::to_value(e.result)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            let success = result == "success";
            ActivityRow {
                id: e.id,
                at: e.at,
                summary: if e.summary.is_empty() {
                    format!("ran spark-cli {}", e.command)
                } else {
                    e.summary
                },
                agent: e.agent,
                detail: e.detail,
                can_undo: success && !e.undo.is_empty() && !e.undone,
                undone: e.undone,
                result,
            }
        })
        .collect()
}

/// The user's Undo on an activity row. Says what was left alone because it
/// changed since.
#[tauri::command]
pub fn undo_agent_activity(app: AppHandle, id: u64) -> Result<Vec<String>, String> {
    let state = app
        .try_state::<AgentState>()
        .ok_or("Agents aren't connected to spark-rs.")?;
    let mailbox = app.state::<Mailbox>();
    let (_, skipped) = commands::undo_for_user(&mailbox.live.engine, &state.log, id)?;
    let _ = app.emit("mailbox-changed", ());
    Ok(skipped)
}

#[tauri::command]
pub fn get_agents(
    mailbox: State<'_, Mailbox>,
    prefs: State<'_, PrefsState>,
) -> Result<AgentsView, String> {
    view(&mailbox, &prefs.get().agent_access)
}

#[tauri::command]
pub fn set_agent_access(
    mailbox: State<'_, Mailbox>,
    prefs: State<'_, PrefsState>,
    account: i64,
    access: Access,
) -> Result<AgentsView, String> {
    let saved = prefs.set_agent_access(account, access, || {
        mailbox
            .live
            .engine
            .store()
            .account(account)
            .map(|a| a.is_some())
            .map_err(|e| e.to_string())
    })?;
    view(&mailbox, &saved.agent_access)
}

#[cfg(test)]
mod tests {
    use super::*;
    use spark_store::{AccountKind, NewAccount, Store};

    fn account(store: &Store, email: &str) -> i64 {
        store
            .add_account(&NewAccount {
                kind: AccountKind::Imap,
                email,
                name: "Me",
                label: "Test",
                config: "{}",
            })
            .unwrap()
            .id
    }

    fn prefs(name: &str) -> (PrefsState, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("spark-agent-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (PrefsState::load(dir.clone()), dir)
    }

    fn tool(prefs: &PrefsState, store: &mut Store, name: &str, args: Value) -> tools::Outcome {
        prefs.with_agent_access(|access| {
            tools::call(
                &mut tools::Context {
                    store,
                    access,
                    agent: "Test agent",
                    now: now_ms(),
                },
                name,
                &args,
            )
        })
    }

    #[test]
    fn removing_an_account_revokes_access_before_its_id_is_reused() {
        let mut store = Store::open_in_memory().unwrap();
        let kept = account(&store, "kept@example.test");
        let old = account(&store, "old@example.test");
        let (prefs, dir) = prefs("reused-account");
        for id in [kept, old] {
            prefs
                .set_agent_access(id, Access::Send, || {
                    Ok(store.account(id).unwrap().is_some())
                })
                .unwrap();
        }
        prefs
            .revoke_agent_access_before(old, || {
                // Revocation is on disk even if the process stops during deletion.
                assert!(
                    !PrefsState::load(dir.clone())
                        .get()
                        .agent_access
                        .contains_key(&old)
                );
                store.remove_account(old).map_err(|e| e.to_string())
            })
            .unwrap();
        let new = account(&store, "private@example.test");
        assert_eq!(new, old, "exercise SQLite's reused account id");
        let reloaded = PrefsState::load(dir.clone());
        let listed = tool(&reloaded, &mut store, "list_accounts", json!({}));
        assert!(!listed.is_error, "{}", listed.text);
        let listed: Value = serde_json::from_str(&listed.text).unwrap();
        assert_eq!(listed["accounts"].as_array().unwrap().len(), 1);
        assert_eq!(listed["accounts"][0]["email"], "kept@example.test");
        let denied = tool(
            &reloaded,
            &mut store,
            "send_email",
            json!({ "account": new, "to": "recipient@example.test", "body": "Hello" }),
        );
        assert!(denied.is_error, "a new mailbox must need a new grant");
        assert!(denied.text.contains("no access"), "{}", denied.text);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn startup_removes_legacy_grants_before_an_account_reuses_the_id() {
        let mut store = Store::open_in_memory().unwrap();
        let old = account(&store, "old@example.test");
        let (prefs, dir) = prefs("legacy-grants");
        prefs
            .update(|p| {
                p.agent_access.insert(old, Access::Send);
            })
            .unwrap();
        store.remove_account(old).unwrap();
        let restarted = PrefsState::load(dir.clone());
        let ids: Vec<_> = store
            .accounts()
            .unwrap()
            .into_iter()
            .map(|a| a.id)
            .collect();
        restarted.retain_agent_accounts(&ids).unwrap();
        let new = account(&store, "private@example.test");
        assert_eq!(new, old);
        let reloaded = PrefsState::load(dir.clone());
        let listed = tool(&reloaded, &mut store, "list_accounts", json!({}));
        let listed: Value = serde_json::from_str(&listed.text).unwrap();
        assert_eq!(listed["accounts"], json!([]));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_revocation_save_keeps_the_account_in_place() {
        let store = Store::open_in_memory().unwrap();
        let id = account(&store, "old@example.test");
        let (prefs, dir) = prefs("failed-revocation");
        prefs
            .set_agent_access(id, Access::Send, || Ok(true))
            .unwrap();
        // A directory where the temporary file should be makes the write fail.
        std::fs::create_dir(dir.join("prefs.json.tmp")).unwrap();
        let result = prefs
            .revoke_agent_access_before(id, || store.remove_account(id).map_err(|e| e.to_string()));
        assert!(result.is_err());
        assert!(store.account(id).unwrap().is_some());
        assert!(!prefs.get().agent_access.contains_key(&id));
        assert_eq!(
            PrefsState::load(dir.clone()).get().agent_access[&id],
            Access::Send
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn adding_accounts_requires_persisting_a_failed_startup_cleanup() {
        let store = Store::open_in_memory().unwrap();
        let old = account(&store, "old@example.test");
        let (prefs, dir) = prefs("failed-startup-cleanup");
        prefs
            .set_agent_access(old, Access::Send, || Ok(true))
            .unwrap();
        store.remove_account(old).unwrap();
        std::fs::create_dir(dir.join("prefs.json.tmp")).unwrap();
        assert!(prefs.retain_agent_accounts(&[]).is_err());
        assert!(prefs.get().agent_access.is_empty());
        assert_eq!(
            PrefsState::load(dir.clone()).get().agent_access[&old],
            Access::Send
        );
        // The in-memory prune must not hide the failed disk write on retry.
        assert!(prefs.prepare_account_add(|| Ok(vec![])).is_err());
        std::fs::remove_dir(dir.join("prefs.json.tmp")).unwrap();
        prefs.prepare_account_add(|| Ok(vec![])).unwrap();
        let new = account(&store, "private@example.test");
        assert_eq!(new, old);
        assert!(PrefsState::load(dir.clone()).get().agent_access.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn grant_edits_cannot_leave_permissions_for_an_absent_account() {
        let store = Store::open_in_memory().unwrap();
        let (prefs, dir) = prefs("absent-account");
        let result =
            prefs.set_agent_access(1, Access::Send, || Ok(store.account(1).unwrap().is_some()));
        assert_eq!(result.unwrap_err(), "That account was removed.");
        assert!(prefs.get().agent_access.is_empty());
        assert!(PrefsState::load(dir.clone()).get().agent_access.is_empty());
        assert!(!dir.exists());
    }

    #[test]
    fn levels_include_the_ones_before() {
        assert!(Access::None < Access::Read);
        assert!(Access::Read < Access::Triage && Access::Triage < Access::Send);
        assert_eq!(serde_json::to_value(Access::Triage).unwrap(), "triage");
        assert_eq!(Access::None.label(), "no access");
        assert_eq!(Access::Send.label(), "send access");
    }

    #[test]
    fn agents_get_readable_names() {
        assert_eq!(agent_name("claude-code"), "Claude Code");
        assert_eq!(agent_name("codex-mcp-client"), "Codex");
        assert_eq!(agent_name("Hermes Agent"), "Hermes");
        assert_eq!(agent_name("claude-ai"), "Claude");
        assert_eq!(agent_name(""), "An agent");
        assert_eq!(agent_name("goose"), "goose");
    }
}
