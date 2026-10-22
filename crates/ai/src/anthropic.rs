//! Anthropic's Messages API (`POST /v1/messages`, streamed).

use serde_json::{Value, json};

use crate::{AiError, ChatRequest, Completion, ProviderConfig, StopReason, Usage, sse};

pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
const API_VERSION: &str = "2023-06-01";
/// Lets the API retry a declined request on Anthropic's recommended fallback
/// model within the same call.
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

/// Models that accept `fallbacks: "default"`. It's only offered on Anthropic's
/// own API, so requests to other base URLs (proxies, gateways) never send it.
fn supports_default_fallback(model: &str) -> bool {
    matches!(
        model,
        "claude-opus-5-5" | "claude-opus-5" | "claude-fable-5-1" | "claude-sonnet-5-5"
    )
}

pub(crate) fn request(
    http: &reqwest::Client,
    config: &ProviderConfig,
    api_key: Option<&str>,
    request: &ChatRequest,
) -> reqwest::RequestBuilder {
    let base = config.base_url.trim_end_matches('/');
    let fallback = base == DEFAULT_BASE_URL && supports_default_fallback(&config.model);
    let mut builder = http
        .post(format!("{base}/v1/messages"))
        .header("anthropic-version", API_VERSION)
        .json(&body(config, request, fallback));
    if let Some(key) = api_key {
        builder = builder.header("x-api-key", key);
    }
    if fallback {
        builder = builder.header("anthropic-beta", FALLBACK_BETA);
    }
    builder
}

pub(crate) fn models_request(
    http: &reqwest::Client,
    config: &ProviderConfig,
    api_key: Option<&str>,
) -> reqwest::RequestBuilder {
    let base = config.base_url.trim_end_matches('/');
    let mut builder = http
        .get(format!("{base}/v1/models?limit=100"))
        .header("anthropic-version", API_VERSION);
    if let Some(key) = api_key {
        builder = builder.header("x-api-key", key);
    }
    builder
}

/// Thinking is left at the model default (adaptive on current models), so
/// effort is the knob for depth and cost.
fn body(config: &ProviderConfig, request: &ChatRequest, fallback: bool) -> Value {
    let mut body = json!({
        "model": config.model,
        "max_tokens": request.max_tokens,
        "stream": true,
        "messages": request.messages,
    });
    if let Some(system) = &request.system {
        body["system"] = json!(system);
    }
    if let Some(effort) = config.effort {
        body["output_config"] = json!({ "effort": effort });
    }
    if fallback {
        body["fallbacks"] = json!("default");
    }
    body
}

#[derive(Debug, Default)]
pub(crate) struct Decoder {
    text: String,
    stop_reason: Option<StopReason>,
    usage: Usage,
    model: Option<String>,
}

impl Decoder {
    /// Returns the text this event added, if any. Thinking blocks, pings and
    /// fallback markers carry no answer text and are skipped.
    pub fn handle(&mut self, event: &sse::Event) -> Result<Option<String>, AiError> {
        let v: Value = serde_json::from_str(&event.data)
            .map_err(|e| AiError::Protocol(format!("bad event from Anthropic: {e}")))?;
        match v["type"].as_str() {
            Some("message_start") => {
                self.model = v["message"]["model"].as_str().map(str::to_owned);
                self.usage.input_tokens =
                    v["message"]["usage"]["input_tokens"].as_u64().unwrap_or(0);
            }
            Some("content_block_delta") if v["delta"]["type"] == "text_delta" => {
                let text = v["delta"]["text"].as_str().unwrap_or_default();
                self.text.push_str(text);
                return Ok(Some(text.to_owned()));
            }
            Some("message_delta") => {
                if let Some(reason) = v["delta"]["stop_reason"].as_str() {
                    self.stop_reason = Some(stop_reason(reason, &v["delta"]["stop_details"]));
                }
                if let Some(tokens) = v["usage"]["output_tokens"].as_u64() {
                    self.usage.output_tokens = tokens;
                }
            }
            Some("error") => {
                let message = v["error"]["message"].as_str().unwrap_or("stream error");
                return Err(AiError::Stream(message.to_owned()));
            }
            _ => {}
        }
        Ok(None)
    }

    pub fn finish(self) -> Result<Completion, AiError> {
        let stop_reason = self.stop_reason.ok_or_else(|| {
            AiError::Protocol("Anthropic stream ended before the message did".into())
        })?;
        // A refusal can arrive after partial output, which must not be used.
        let text = if matches!(stop_reason, StopReason::Refusal { .. }) {
            String::new()
        } else {
            self.text
        };
        Ok(Completion {
            text,
            stop_reason,
            usage: self.usage,
            model: self.model,
        })
    }
}

fn stop_reason(reason: &str, details: &Value) -> StopReason {
    match reason {
        "end_turn" | "stop_sequence" => StopReason::EndTurn,
        "max_tokens" => StopReason::MaxTokens,
        "refusal" => StopReason::Refusal {
            category: details["category"].as_str().map(str::to_owned),
        },
        other => StopReason::Other(other.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Effort, Message, ProviderKind};

    fn config(base_url: &str, model: &str) -> ProviderConfig {
        ProviderConfig {
            kind: ProviderKind::Anthropic,
            base_url: base_url.into(),
            model: model.into(),
            effort: Some(Effort::Medium),
        }
    }

    fn chat() -> ChatRequest {
        ChatRequest {
            system: Some("Be brief.".into()),
            messages: vec![Message::user("Summarize this thread")],
            max_tokens: 2000,
        }
    }

    #[test]
    fn body_enables_fallback_only_on_anthropic_api_for_supported_models() {
        let official = body(&config(DEFAULT_BASE_URL, "claude-opus-5-5"), &chat(), true);
        assert_eq!(official["fallbacks"], "default");
        assert_eq!(official["output_config"]["effort"], "medium");
        assert_eq!(official["system"], "Be brief.");
        assert_eq!(official["messages"][0]["role"], "user");
        assert!(supports_default_fallback("claude-opus-5-5"));
        assert!(!supports_default_fallback("claude-haiku-5-5"));
    }

    fn decode(stream: &str) -> Result<(String, Completion), AiError> {
        let mut parser = sse::Parser::default();
        let mut decoder = Decoder::default();
        let mut streamed = String::new();
        for event in parser.push(stream.as_bytes()) {
            if let Some(text) = decoder.handle(&event)? {
                streamed.push_str(&text);
            }
        }
        Ok((streamed, decoder.finish()?))
    }

    #[test]
    fn decodes_text_usage_and_stop_reason() {
        let (streamed, done) = decode(concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-5-5\",\"usage\":{\"input_tokens\":42}}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"x\"}}\n\n",
            "event: ping\ndata: {\"type\":\"ping\"}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ))
        .unwrap();
        assert_eq!(streamed, "Hello world");
        assert_eq!(done.text, "Hello world");
        assert_eq!(done.stop_reason, StopReason::EndTurn);
        assert_eq!(
            done.usage,
            Usage {
                input_tokens: 42,
                output_tokens: 7
            }
        );
        assert_eq!(done.model.as_deref(), Some("claude-opus-5-5"));
    }

    #[test]
    fn refusal_discards_partial_text() {
        let (_, done) = decode(concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-5-5\",\"usage\":{\"input_tokens\":5}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Partial\"}}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\",\"stop_details\":{\"type\":\"refusal\",\"category\":\"cyber\"}},\"usage\":{\"output_tokens\":1}}\n\n",
        ))
        .unwrap();
        assert_eq!(done.text, "");
        assert_eq!(
            done.stop_reason,
            StopReason::Refusal {
                category: Some("cyber".into())
            }
        );
    }

    #[test]
    fn error_event_is_an_error() {
        let result = decode(
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n",
        );
        assert!(matches!(result, Err(AiError::Stream(m)) if m == "Overloaded"));
    }
}
