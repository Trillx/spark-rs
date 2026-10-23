//! Bring-your-own-key AI: the provider settings, the API key in the OS
//! credential store, +AI Summary and the composer's +AI (Quick +AI Replies
//! included). The key stays on this side; the UI only learns whether one is
//! stored.

mod compose;
mod email;
pub(crate) mod keys;
mod settings;
mod stream;
mod summary;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use spark_ai::{AiClient, AiError, ChatRequest, ProviderConfig};
use tauri::async_runtime::JoinHandle;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State};

use crate::mailbox::Mailbox;
use compose::{ComposeAction, Reference};
pub use keys::Keys;
use settings::{AiSettings, Preset};
use stream::AiEvent;
use summary::SummaryStyle;

pub struct AiState {
    config_dir: PathBuf,
    keys: Keys,
    /// Serializes writes to `ai.json`.
    saving: Mutex<()>,
    /// What's streaming, so it can be stopped or replaced.
    runs: Mutex<HashMap<RunKey, Run>>,
    next_run: AtomicU64,
}

/// One run at a time for each of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum RunKey {
    /// A message's summary.
    Summary(i64),
    /// What AI writes in a composer, by the UI's number for it.
    Composer(u64),
}

struct Run {
    run: u64,
    task: JoinHandle<()>,
}

/// What the settings page sees: never the key, only whether one is stored.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSettingsView {
    preset: Preset,
    config: ProviderConfig,
    has_key: bool,
}

/// Why a client couldn't be made.
enum Unready {
    /// Something still has to be filled in; the message says what.
    NotConfigured(String),
    Failed(String),
}

impl Unready {
    fn into_message(self) -> String {
        match self {
            Unready::NotConfigured(message) | Unready::Failed(message) => message,
        }
    }
}

impl AiState {
    pub fn new(config_dir: PathBuf, keys: Keys) -> Self {
        Self {
            config_dir,
            keys,
            saving: Mutex::new(()),
            runs: Mutex::default(),
            next_run: AtomicU64::new(0),
        }
    }

    fn view(&self, settings: AiSettings) -> AiSettingsView {
        AiSettingsView {
            has_key: self.keys.has(settings.preset),
            preset: settings.preset,
            config: settings.config,
        }
    }

    fn save(
        &self,
        preset: Preset,
        config: ProviderConfig,
        api_key: Option<String>,
    ) -> Result<AiSettingsView, String> {
        let settings = AiSettings {
            preset,
            config: preset.normalize(config)?,
        };
        let _guard = self.saving.lock().unwrap();
        if let Some(key) = api_key {
            self.keys.set(preset, &key)?;
        }
        settings.save(&self.config_dir)?;
        Ok(self.view(settings))
    }

    /// A client for the saved settings, plus its key so errors can be scrubbed of it.
    fn client(&self, need_model: bool) -> Result<(AiClient, Option<String>), Unready> {
        let Some(settings) = AiSettings::load(&self.config_dir).map_err(Unready::Failed)? else {
            return Err(Unready::NotConfigured(
                "Choose a provider and add your API key in Settings → AI.".into(),
            ));
        };
        let key = self.keys.get(settings.preset).map_err(Unready::Failed)?;
        if let Some(missing) = settings.missing(key.is_some(), need_model) {
            return Err(Unready::NotConfigured(missing));
        }
        let client = AiClient::new(settings.config, key.clone())
            .map_err(|e| Unready::Failed(describe(&e)))?;
        Ok((client, key))
    }

    async fn list_models(&self) -> Result<Vec<String>, String> {
        let (client, key) = self.client(false).map_err(Unready::into_message)?;
        let mut models = client
            .list_models()
            .await
            .map_err(|e| redact(describe(&e), key.as_deref()))?;
        models.sort();
        models.dedup();
        Ok(models)
    }

    /// Streams `request` over `on_event` as the run for `key`, replacing one
    /// already running. Without a usable provider it sends the one event that
    /// says why.
    fn start(&self, app: AppHandle, key: RunKey, request: ChatRequest, on_event: Channel<AiEvent>) {
        let (client, api_key) = match self.client(true) {
            Ok(ready) => ready,
            Err(unready) => {
                let _ = on_event.send(match unready {
                    Unready::NotConfigured(message) => AiEvent::NotConfigured { message },
                    Unready::Failed(message) => AiEvent::Error { message },
                });
                return;
            }
        };
        self.cancel(key);
        let run = self.next_run.fetch_add(1, Ordering::Relaxed);
        // Held until the handle is stored, so a fast run can't finish before it's registered.
        let mut runs = self.runs.lock().unwrap();
        let task = tauri::async_runtime::spawn(async move {
            let model = client.config().model.clone();
            let result = client
                .complete(&request, |text| {
                    let _ = on_event.send(AiEvent::Delta { text: text.into() });
                })
                .await
                .map_err(|e| redact(describe(&e), api_key.as_deref()));
            let _ = on_event.send(AiEvent::finished(result, &model));
            app.state::<AiState>().finish(key, run);
        });
        runs.insert(key, Run { run, task });
    }

    fn cancel(&self, key: RunKey) {
        if let Some(running) = self.runs.lock().unwrap().remove(&key) {
            running.task.abort();
        }
    }

    /// Drops a finished run's handle, unless a newer run replaced it.
    fn finish(&self, key: RunKey, run: u64) {
        let mut runs = self.runs.lock().unwrap();
        if runs.get(&key).is_some_and(|r| r.run == run) {
            runs.remove(&key);
        }
    }
}

/// `None` until AI settings are saved for the first time.
#[tauri::command]
pub async fn get_ai_settings(state: State<'_, AiState>) -> Result<Option<AiSettingsView>, String> {
    Ok(AiSettings::load(&state.config_dir)?.map(|s| state.view(s)))
}

/// `api_key`: `None` keeps the stored key, `Some("")` deletes it.
#[tauri::command]
pub async fn save_ai_settings(
    state: State<'_, AiState>,
    preset: Preset,
    config: ProviderConfig,
    api_key: Option<String>,
) -> Result<AiSettingsView, String> {
    state.save(preset, config, api_key)
}

/// Lists the provider's models with the saved settings and key, which proves
/// both work. Returns the model ids.
#[tauri::command]
pub async fn test_ai_connection(state: State<'_, AiState>) -> Result<Vec<String>, String> {
    state.list_models().await
}

#[tauri::command]
pub async fn list_ai_models(state: State<'_, AiState>) -> Result<Vec<String>, String> {
    state.list_models().await
}

/// Starts summarizing a message and returns at once; the summary streams
/// over `on_event`. A new request for the same message replaces a running one.
/// `style` defaults to Short.
#[tauri::command]
pub async fn summarize_message(
    app: AppHandle,
    state: State<'_, AiState>,
    mailbox: State<'_, Mailbox>,
    id: i64,
    style: Option<SummaryStyle>,
    on_event: Channel<AiEvent>,
) -> Result<(), String> {
    let mail = mailbox
        .with_body(id)
        .ok_or_else(|| format!("no message with id {id}"))?;
    let request = summary::summary_request(&mail, style.unwrap_or_default());
    state.start(app, RunKey::Summary(id), request, on_event);
    Ok(())
}

/// Stops a running summary. The UI keeps whatever text already arrived.
#[tauri::command]
pub fn cancel_summary(state: State<'_, AiState>, id: i64) {
    state.cancel(RunKey::Summary(id));
}

/// Starts AI writing for the UI's composer number `composer` and returns at
/// once; the text streams over `on_event`. `account` is the draft's, and
/// `reference` the email it replies to or forwards. `signed`: the draft has a
/// signature below, so what's written doesn't sign off. A new request for
/// the same composer replaces a running one.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn compose_with_ai(
    app: AppHandle,
    state: State<'_, AiState>,
    mailbox: State<'_, Mailbox>,
    composer: u64,
    action: ComposeAction,
    account: i64,
    reference: Option<Reference>,
    signed: Option<bool>,
    on_event: Channel<AiEvent>,
) -> Result<(), String> {
    let email = match reference {
        Some(r) => Some((
            mailbox
                .with_body(r.id)
                .ok_or_else(|| format!("no message with id {}", r.id))?,
            r.relation,
        )),
        None => None,
    };
    let name = mailbox.user_name(account);
    let request = compose::compose_request(
        &action,
        email.as_ref().map(|(mail, relation)| (mail, *relation)),
        name.as_deref(),
        signed.unwrap_or(false),
    )?;
    state.start(app, RunKey::Composer(composer), request, on_event);
    Ok(())
}

/// Stops AI writing in a composer. The UI decides what to keep.
#[tauri::command]
pub fn cancel_compose(state: State<'_, AiState>, composer: u64) {
    state.cancel(RunKey::Composer(composer));
}

/// AI errors in words for the UI.
fn describe(error: &AiError) -> String {
    match error {
        AiError::Http {
            status: status @ (401 | 403),
            message,
        } => format!("The provider didn't accept the API key (HTTP {status}): {message}"),
        AiError::Http {
            status: 404,
            message,
        } => format!("Not found (HTTP 404). Check the base URL and the model. {message}"),
        AiError::Http {
            status: 429,
            message,
        } => format!("Rate limited or out of credit (HTTP 429): {message}"),
        AiError::Network(e) => {
            // reqwest's own message is generic; the innermost cause says what happened.
            let mut cause: &dyn std::error::Error = e;
            while let Some(next) = cause.source() {
                cause = next;
            }
            let host = e
                .url()
                .and_then(|url| Some(format!("{}{}", url.host_str()?, port_suffix(url.port()))))
                .unwrap_or_else(|| "the AI provider".into());
            format!("Couldn't reach {host}: {cause}")
        }
        other => capitalize(&other.to_string()),
    }
}

fn port_suffix(port: Option<u16>) -> String {
    port.map(|p| format!(":{p}")).unwrap_or_default()
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Some providers echo the key in auth errors; it must not reach the UI.
fn redact(message: String, key: Option<&str>) -> String {
    match key {
        Some(key) if key.len() >= 8 && message.contains(key) => message.replace(key, "[API key]"),
        _ => message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keyring_core::mock;

    fn state(name: &str) -> AiState {
        let dir = std::env::temp_dir().join(format!("spark-rs-ai-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        AiState::new(dir, Keys::with_store(mock::Store::new().unwrap()))
    }

    #[test]
    fn saving_keeps_the_key_out_of_the_file_and_the_view() {
        let state = state("save");
        let view = state
            .save(
                Preset::Anthropic,
                ProviderConfig::anthropic(),
                Some("sk-ant-very-secret".into()),
            )
            .unwrap();
        assert!(view.has_key);
        let file = std::fs::read_to_string(state.config_dir.join("ai.json")).unwrap();
        assert!(!file.contains("sk-ant-very-secret"));
        let json = serde_json::to_string(&view).unwrap();
        assert!(!json.contains("sk-ant-very-secret"));
        assert!(json.contains("\"hasKey\":true"));

        // None keeps the key; another preset has its own.
        let view = state
            .save(Preset::Anthropic, ProviderConfig::anthropic(), None)
            .unwrap();
        assert!(view.has_key);
        let view = state
            .save(Preset::OpenAi, Preset::OpenAi.defaults(), None)
            .unwrap();
        assert!(!view.has_key);
        assert!(state.keys.has(Preset::Anthropic));

        // Some("") deletes it.
        let view = state
            .save(
                Preset::Anthropic,
                ProviderConfig::anthropic(),
                Some(String::new()),
            )
            .unwrap();
        assert!(!view.has_key);
        std::fs::remove_dir_all(&state.config_dir).unwrap();
    }

    #[test]
    fn an_invalid_config_saves_nothing() {
        let state = state("invalid");
        let config = ProviderConfig::openai_compatible("ftp://example.com", "m");
        assert!(
            state
                .save(Preset::Custom, config, Some("key-1234567".into()))
                .is_err()
        );
        assert!(!state.keys.has(Preset::Custom));
        assert!(!state.config_dir.join("ai.json").exists());
    }

    #[test]
    fn a_client_needs_saved_settings_and_a_key() {
        let state = state("client");
        assert!(matches!(state.client(true), Err(Unready::NotConfigured(_))));

        state
            .save(Preset::Anthropic, ProviderConfig::anthropic(), None)
            .unwrap();
        match state.client(true) {
            Err(Unready::NotConfigured(message)) => assert!(message.contains("Anthropic API key")),
            _ => panic!("expected NotConfigured"),
        }

        state
            .save(
                Preset::Custom,
                ProviderConfig::openai_compatible("http://localhost:11434/v1", ""),
                None,
            )
            .unwrap();
        assert!(state.client(false).is_ok());
        assert!(matches!(state.client(true), Err(Unready::NotConfigured(_))));
        std::fs::remove_dir_all(&state.config_dir).unwrap();
    }

    #[test]
    fn describes_errors_readably_without_the_key() {
        let unauthorized = AiError::Http {
            status: 401,
            message: "Incorrect API key provided: sk-proj-abcdefgh".into(),
        };
        let text = redact(describe(&unauthorized), Some("sk-proj-abcdefgh"));
        assert_eq!(
            text,
            "The provider didn't accept the API key (HTTP 401): Incorrect API key provided: [API key]"
        );
        assert_eq!(
            describe(&AiError::Protocol("model list has no `data` array".into())),
            "Unexpected response from the AI provider: model list has no `data` array"
        );
        assert_eq!(redact("short".into(), Some("abc")), "short");
    }
}
