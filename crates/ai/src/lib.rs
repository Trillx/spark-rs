//! Bring-your-own-key AI for spark-rs.
//!
//! Spark sends its AI features through Readdle's servers. spark-rs calls the
//! user's own provider instead: Anthropic's Messages API, or anything that
//! speaks OpenAI's Chat Completions API (OpenAI itself, OpenRouter, Ollama,
//! LM Studio, vLLM, …) via a custom base URL.
//!
//! [`AiClient::complete`] streams one answer. [`AiClient::converse`] is one
//! step of a conversation with tools: the caller runs the tools the model
//! asks for and calls it again with their results.

mod anthropic;
mod openai;
mod sse;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use anthropic::{
    DEFAULT_BASE_URL as ANTHROPIC_BASE_URL, DEFAULT_MODEL as ANTHROPIC_DEFAULT_MODEL,
};
pub use openai::DEFAULT_BASE_URL as OPENAI_BASE_URL;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    Anthropic,
    /// OpenAI or any server implementing its Chat Completions API.
    OpenAiCompatible,
}

/// Reasoning effort for Anthropic models; other providers ignore it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

/// Which provider to call. The API key is kept separately (in the OS
/// credential store) and handed to [`AiClient::new`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    pub base_url: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
}

impl ProviderConfig {
    pub fn anthropic() -> Self {
        Self {
            kind: ProviderKind::Anthropic,
            base_url: ANTHROPIC_BASE_URL.into(),
            model: ANTHROPIC_DEFAULT_MODEL.into(),
            effort: Some(Effort::Medium),
        }
    }

    /// OpenAI or a compatible server, e.g. `http://localhost:11434/v1` for Ollama.
    pub fn openai_compatible(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            kind: ProviderKind::OpenAiCompatible,
            base_url: base_url.into(),
            model: model.into(),
            effort: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatRequest {
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub max_tokens: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    /// The model is waiting for the results of the tools it called.
    ToolUse,
    /// The provider declined. Any text streamed before this must be discarded.
    Refusal {
        category: Option<String>,
    },
    Other(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    /// The full answer; empty after a refusal.
    pub text: String,
    pub stop_reason: StopReason,
    pub usage: Usage,
    /// The model that answered, which can differ from the one requested
    /// after a server-side fallback.
    pub model: Option<String>,
}

/// A tool the model may call during [`AiClient::converse`].
#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    pub name: String,
    /// When to call it and what it does, for the model.
    pub description: String,
    /// JSON Schema for its input object.
    pub input_schema: Value,
}

/// A piece of a [`Turn`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Part {
    Text {
        text: String,
    },
    /// The model asks for a tool. `input` is an object, or the text the model
    /// sent when it wasn't valid JSON (see [`Part::invalid_input`]).
    ToolCall {
        id: String,
        name: String,
        input: Value,
    },
    /// What a tool call returned, sent back in the next user turn.
    ToolResult {
        id: String,
        content: String,
        is_error: bool,
    },
    /// Provider content that has to go back unchanged on later requests,
    /// such as Anthropic's thinking blocks. Other providers leave it out.
    Opaque {
        provider: ProviderKind,
        block: Value,
    },
}

impl Part {
    pub fn text(text: impl Into<String>) -> Self {
        Part::Text { text: text.into() }
    }

    /// For a tool call whose input didn't parse: the error result to send
    /// back, so the model can try again.
    pub fn invalid_input(id: &str, input: &Value) -> Option<Self> {
        if input.is_object() {
            return None;
        }
        let raw = input
            .as_str()
            .map_or_else(|| input.to_string(), str::to_owned);
        Some(Part::ToolResult {
            id: id.to_owned(),
            content: serde_json::json!({ "INVALID_JSON": raw }).to_string(),
            is_error: true,
        })
    }
}

/// One message of a conversation with tools.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub role: Role,
    pub parts: Vec<Part>,
}

impl Turn {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            parts: vec![Part::text(text)],
        }
    }

    /// Its text parts, joined.
    pub fn text(&self) -> String {
        self.parts
            .iter()
            .filter_map(|p| match p {
                Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The tools it calls: id, name and input.
    pub fn tool_calls(&self) -> impl Iterator<Item = (&str, &str, &Value)> {
        self.parts.iter().filter_map(|p| match p {
            Part::ToolCall { id, name, input } => Some((id.as_str(), name.as_str(), input)),
            _ => None,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConversationRequest {
    pub system: Option<String>,
    /// Starts and ends with a user turn; roles alternate.
    pub turns: Vec<Turn>,
    pub tools: Vec<Tool>,
    pub max_tokens: u32,
}

/// The model's next turn.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    /// Empty after a refusal: whatever streamed before it must be discarded.
    pub turn: Turn,
    pub stop_reason: StopReason,
    pub usage: Usage,
    /// The model that answered, which can differ from the one requested.
    pub model: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("couldn't reach the AI provider: {0}")]
    Network(#[from] reqwest::Error),
    #[error("the AI provider returned HTTP {status}: {message}")]
    Http { status: u16, message: String },
    #[error("the AI provider reported an error: {0}")]
    Stream(String),
    #[error("unexpected response from the AI provider: {0}")]
    Protocol(String),
}

pub struct AiClient {
    http: reqwest::Client,
    config: ProviderConfig,
    api_key: Option<String>,
}

impl AiClient {
    pub fn new(config: ProviderConfig, api_key: Option<String>) -> Result<Self, AiError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("spark-rs/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            config,
            api_key,
        })
    }

    pub fn config(&self) -> &ProviderConfig {
        &self.config
    }

    /// Streams a completion, calling `on_text` with each piece of the answer
    /// as it arrives. If the result is a [`StopReason::Refusal`], whatever was
    /// already shown must be thrown away.
    pub async fn complete(
        &self,
        request: &ChatRequest,
        mut on_text: impl FnMut(&str),
    ) -> Result<Completion, AiError> {
        let key = self.api_key.as_deref();
        let (builder, mut decoder) = match self.config.kind {
            ProviderKind::Anthropic => (
                anthropic::request(&self.http, &self.config, key, request),
                Decoder::Anthropic(Default::default()),
            ),
            ProviderKind::OpenAiCompatible => (
                openai::request(&self.http, &self.config, key, request),
                Decoder::OpenAi(Default::default()),
            ),
        };
        stream(builder, |event| {
            if let Some(text) = decoder.handle(event)? {
                on_text(&text);
            }
            Ok(())
        })
        .await?;
        decoder.finish()
    }

    /// Streams the model's next turn of a conversation with tools, calling
    /// `on_text` with each piece of its text. When it stops for
    /// [`StopReason::ToolUse`], run its tool calls and call this again with
    /// the reply's turn and a user turn of their results appended. After a
    /// refusal the streamed text must be thrown away.
    pub async fn converse(
        &self,
        request: &ConversationRequest,
        mut on_text: impl FnMut(&str),
    ) -> Result<Reply, AiError> {
        let key = self.api_key.as_deref();
        match self.config.kind {
            ProviderKind::Anthropic => {
                let mut decoder = anthropic::TurnDecoder::default();
                let builder =
                    anthropic::conversation_request(&self.http, &self.config, key, request);
                stream(builder, |event| {
                    if let Some(text) = decoder.handle(event)? {
                        on_text(&text);
                    }
                    Ok(())
                })
                .await?;
                decoder.finish()
            }
            ProviderKind::OpenAiCompatible => {
                let mut decoder = openai::TurnDecoder::default();
                let builder = openai::conversation_request(&self.http, &self.config, key, request);
                stream(builder, |event| {
                    if let Some(text) = decoder.handle(event)? {
                        on_text(&text);
                    }
                    Ok(())
                })
                .await?;
                decoder.finish()
            }
        }
    }

    /// Model ids the provider offers, for the settings picker.
    pub async fn list_models(&self) -> Result<Vec<String>, AiError> {
        let key = self.api_key.as_deref();
        let builder = match self.config.kind {
            ProviderKind::Anthropic => anthropic::models_request(&self.http, &self.config, key),
            ProviderKind::OpenAiCompatible => openai::models_request(&self.http, &self.config, key),
        };
        let body: serde_json::Value = check_status(builder.send().await?).await?.json().await?;
        let models = body["data"]
            .as_array()
            .ok_or_else(|| AiError::Protocol("model list has no `data` array".into()))?;
        Ok(models
            .iter()
            .filter_map(|m| m["id"].as_str().map(str::to_owned))
            .collect())
    }
}

enum Decoder {
    Anthropic(anthropic::Decoder),
    OpenAi(openai::Decoder),
}

impl Decoder {
    fn handle(&mut self, event: &sse::Event) -> Result<Option<String>, AiError> {
        match self {
            Decoder::Anthropic(d) => d.handle(event),
            Decoder::OpenAi(d) => d.handle(event),
        }
    }

    fn finish(self) -> Result<Completion, AiError> {
        match self {
            Decoder::Anthropic(d) => d.finish(),
            Decoder::OpenAi(d) => d.finish(),
        }
    }
}

/// Sends `builder` and hands each server-sent event to `handle`.
async fn stream(
    builder: reqwest::RequestBuilder,
    mut handle: impl FnMut(&sse::Event) -> Result<(), AiError>,
) -> Result<(), AiError> {
    let response = check_status(builder.send().await?).await?;
    let mut parser = sse::Parser::default();
    let mut body = response.bytes_stream();
    while let Some(chunk) = body.next().await {
        for event in parser.push(&chunk?) {
            handle(&event)?;
        }
    }
    Ok(())
}

/// Both APIs report failures as `{"error": {"message": …}}`.
async fn check_status(response: reqwest::Response) -> Result<reqwest::Response, AiError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.text().await.unwrap_or_default();
    let message = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| body.chars().take(500).collect());
    Err(AiError::Http {
        status: status.as_u16(),
        message,
    })
}
