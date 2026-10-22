//! AI agents on this computer (Claude Code, Codex, …) using the user's mail.
//! Agents run `spark-cli`, which reaches this app over a loopback connection
//! (the protocol is in the `spark-agent` crate). The app runs the tools in
//! `tools.rs` against the access the user chose per account in Settings →
//! Agents, and tells the UI what agents did.

mod tools;

use std::collections::BTreeMap;
use std::net::Ipv4Addr;

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

/// Listens for `spark-cli` and writes where to find this app to
/// `agent-endpoint.json` in the local data folder.
pub fn start(app: &AppHandle) -> Result<(), String> {
    let path = app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?
        .join(ENDPOINT_FILE);
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
                    async move { handle(&app, request) }
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

fn handle(app: &AppHandle, request: Request) -> Response {
    match request.method.as_str() {
        "ping" => Response::Result(json!({
            "app": "spark-rs",
            "version": app.package_info().version.to_string(),
        })),
        "tools/list" => Response::Result(tools::catalog()),
        "tools/call" => {
            let name = request.params["name"].as_str().unwrap_or_default();
            let args = match &request.params["arguments"] {
                Value::Null => json!({}),
                args => args.clone(),
            };
            let outcome = run(app, &agent_name(&request.client), name, &args);
            Response::Result(json!({
                "content": [{ "type": "text", "text": outcome.text }],
                "isError": outcome.is_error,
            }))
        }
        other => Response::Error(format!("spark-rs doesn't know {other}.")),
    }
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
    let access = app.state::<PrefsState>().get().agent_access;
    let mailbox = app.state::<Mailbox>();
    let outcome = {
        let mut store = mailbox.live.engine.store();
        let mut cx = tools::Context {
            store: &mut store,
            access: &access,
            agent,
            now: now_ms(),
        };
        tools::call(&mut cx, name, args)
    };
    match &outcome.effect {
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
    outcome
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
    let saved = prefs.update(|p| {
        if access == Access::None {
            p.agent_access.remove(&account);
        } else {
            p.agent_access.insert(account, access);
        }
    })?;
    view(&mailbox, &saved.agent_access)
}

#[cfg(test)]
mod tests {
    use super::*;

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
