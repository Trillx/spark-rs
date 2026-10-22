//! OpenAI's Chat Completions API (`POST {base}/chat/completions`, streamed).
//!
//! This is the shape most other providers and local servers implement
//! (OpenRouter, Groq, Together, Ollama, LM Studio, vLLM, …), so one client
//! covers all of them via the base URL.

use serde_json::{Value, json};

use crate::{AiError, ChatRequest, Completion, ProviderConfig, StopReason, Usage, sse};

pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

pub(crate) fn request(
    http: &reqwest::Client,
    config: &ProviderConfig,
    api_key: Option<&str>,
    request: &ChatRequest,
) -> reqwest::RequestBuilder {
    let base = config.base_url.trim_end_matches('/');
    let builder = http.post(format!("{base}/chat/completions")).json(&body(
        config,
        request,
        base == DEFAULT_BASE_URL,
    ));
    authorize(builder, api_key)
}

pub(crate) fn models_request(
    http: &reqwest::Client,
    config: &ProviderConfig,
    api_key: Option<&str>,
) -> reqwest::RequestBuilder {
    let base = config.base_url.trim_end_matches('/');
    authorize(http.get(format!("{base}/models")), api_key)
}

/// Local servers often need no key at all.
fn authorize(builder: reqwest::RequestBuilder, api_key: Option<&str>) -> reqwest::RequestBuilder {
    match api_key {
        Some(key) => builder.bearer_auth(key),
        None => builder,
    }
}

fn body(config: &ProviderConfig, request: &ChatRequest, official: bool) -> Value {
    let mut messages = Vec::with_capacity(request.messages.len() + 1);
    if let Some(system) = &request.system {
        messages.push(json!({ "role": "system", "content": system }));
    }
    messages.extend(request.messages.iter().map(|m| json!(m)));

    let mut body = json!({ "model": config.model, "stream": true, "messages": messages });
    // OpenAI itself wants `max_completion_tokens` (its reasoning models reject
    // `max_tokens`); compatible servers generally only understand `max_tokens`.
    if official {
        body["max_completion_tokens"] = json!(request.max_tokens);
        body["stream_options"] = json!({ "include_usage": true });
    } else {
        body["max_tokens"] = json!(request.max_tokens);
    }
    body
}

#[derive(Debug, Default)]
pub(crate) struct Decoder {
    text: String,
    finish_reason: Option<String>,
    usage: Usage,
    model: Option<String>,
}

impl Decoder {
    pub fn handle(&mut self, event: &sse::Event) -> Result<Option<String>, AiError> {
        if event.data == "[DONE]" {
            return Ok(None);
        }
        let v: Value = serde_json::from_str(&event.data)
            .map_err(|e| AiError::Protocol(format!("bad chunk from provider: {e}")))?;
        if let Some(error) = v.get("error") {
            let message = error["message"].as_str().unwrap_or("stream error");
            return Err(AiError::Stream(message.to_owned()));
        }
        if let Some(model) = v["model"].as_str() {
            self.model = Some(model.to_owned());
        }
        if v["usage"].is_object() {
            self.usage = Usage {
                input_tokens: v["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
                output_tokens: v["usage"]["completion_tokens"].as_u64().unwrap_or(0),
            };
        }
        let choice = &v["choices"][0];
        if let Some(reason) = choice["finish_reason"].as_str() {
            self.finish_reason = Some(reason.to_owned());
        }
        match choice["delta"]["content"].as_str() {
            Some(text) if !text.is_empty() => {
                self.text.push_str(text);
                Ok(Some(text.to_owned()))
            }
            _ => Ok(None),
        }
    }

    pub fn finish(self) -> Result<Completion, AiError> {
        let reason = self
            .finish_reason
            .ok_or_else(|| AiError::Protocol("stream ended without a finish reason".into()))?;
        let stop_reason = match reason.as_str() {
            "stop" => StopReason::EndTurn,
            "length" => StopReason::MaxTokens,
            "content_filter" => StopReason::Refusal {
                category: Some(reason),
            },
            _ => StopReason::Other(reason),
        };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, ProviderKind};

    fn config(base_url: &str) -> ProviderConfig {
        ProviderConfig {
            kind: ProviderKind::OpenAiCompatible,
            base_url: base_url.into(),
            model: "some-model".into(),
            effort: None,
        }
    }

    fn chat() -> ChatRequest {
        ChatRequest {
            system: Some("Be brief.".into()),
            messages: vec![Message::user("Hi")],
            max_tokens: 500,
        }
    }

    #[test]
    fn system_prompt_becomes_first_message() {
        let body = body(&config("http://localhost:11434/v1"), &chat(), false);
        assert_eq!(
            body["messages"][0],
            json!({ "role": "system", "content": "Be brief." })
        );
        assert_eq!(
            body["messages"][1],
            json!({ "role": "user", "content": "Hi" })
        );
        assert_eq!(body["max_tokens"], 500);
        assert!(body.get("max_completion_tokens").is_none());
    }

    #[test]
    fn openai_itself_gets_max_completion_tokens_and_usage() {
        let body = body(&config(DEFAULT_BASE_URL), &chat(), true);
        assert_eq!(body["max_completion_tokens"], 500);
        assert!(body.get("max_tokens").is_none());
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    fn decode(stream: &str) -> Completion {
        let mut parser = sse::Parser::default();
        let mut decoder = Decoder::default();
        for event in parser.push(stream.as_bytes()) {
            decoder.handle(&event).unwrap();
        }
        decoder.finish().unwrap()
    }

    #[test]
    fn decodes_chunks_until_done() {
        let done = decode(concat!(
            "data: {\"model\":\"m\",\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\n",
            "data: {\"model\":\"m\",\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
            "data: {\"model\":\"m\",\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"model\":\"m\",\"choices\":[],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":2}}\n\n",
            "data: [DONE]\n\n",
        ));
        assert_eq!(done.text, "Hello");
        assert_eq!(done.stop_reason, StopReason::EndTurn);
        assert_eq!(
            done.usage,
            Usage {
                input_tokens: 9,
                output_tokens: 2
            }
        );
    }

    #[test]
    fn content_filter_is_a_refusal() {
        let done = decode(concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Par\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"content_filter\"}]}\n\n",
            "data: [DONE]\n\n",
        ));
        assert_eq!(done.text, "");
        assert!(matches!(done.stop_reason, StopReason::Refusal { .. }));
    }
}
