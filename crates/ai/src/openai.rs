//! OpenAI's Chat Completions API (`POST {base}/chat/completions`, streamed).
//!
//! This is the shape most other providers and local servers implement
//! (OpenRouter, Groq, Together, Ollama, LM Studio, vLLM, …), so one client
//! covers all of them via the base URL.

use serde_json::{Value, json};

use crate::{
    AiError, ChatRequest, Completion, ConversationRequest, Part, ProviderConfig, Reply, Role,
    StopReason, Turn, Usage, sse,
};

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

pub(crate) fn conversation_request(
    http: &reqwest::Client,
    config: &ProviderConfig,
    api_key: Option<&str>,
    request: &ConversationRequest,
) -> reqwest::RequestBuilder {
    let base = config.base_url.trim_end_matches('/');
    let builder = http
        .post(format!("{base}/chat/completions"))
        .json(&conversation_body(
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

fn conversation_body(
    config: &ProviderConfig,
    request: &ConversationRequest,
    official: bool,
) -> Value {
    let chat = ChatRequest {
        system: request.system.clone(),
        messages: Vec::new(),
        max_tokens: request.max_tokens,
    };
    let mut body = body(config, &chat, official);
    let mut messages: Vec<Value> = body["messages"].as_array().cloned().unwrap_or_default();
    for turn in &request.turns {
        messages.extend(turn_messages(turn));
    }
    body["messages"] = json!(messages);
    if !request.tools.is_empty() {
        body["tools"] = request
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": { "name": t.name, "description": t.description, "parameters": t.input_schema },
                })
            })
            .collect();
    }
    body
}

/// A turn as Chat Completions messages: tool results are messages of their
/// own, and an assistant's tool calls ride on its message.
fn turn_messages(turn: &Turn) -> Vec<Value> {
    let text = turn.text();
    match turn.role {
        Role::User => {
            let mut messages: Vec<Value> = turn
                .parts
                .iter()
                .filter_map(|p| match p {
                    Part::ToolResult {
                        id,
                        content,
                        is_error,
                    } => Some(json!({
                        "role": "tool",
                        "tool_call_id": id,
                        "content": if *is_error { format!("Error: {content}") } else { content.clone() },
                    })),
                    _ => None,
                })
                .collect();
            if !text.is_empty() {
                messages.push(json!({ "role": "user", "content": text }));
            }
            messages
        }
        Role::Assistant => {
            let calls: Vec<Value> = turn
                .tool_calls()
                .map(|(id, name, input)| {
                    let arguments = if input.is_object() { input.to_string() } else { "{}".into() };
                    json!({ "id": id, "type": "function", "function": { "name": name, "arguments": arguments } })
                })
                .collect();
            let mut message = json!({
                "role": "assistant",
                "content": if text.is_empty() { Value::Null } else { json!(text) },
            });
            if !calls.is_empty() {
                message["tool_calls"] = json!(calls);
            }
            vec![message]
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Decoder {
    text: String,
    /// The model declined in words of its own (`delta.refusal`), which
    /// can end in an ordinary "stop".
    refused: bool,
    finish_reason: Option<String>,
    usage: Usage,
    model: Option<String>,
}

impl Decoder {
    pub fn handle(&mut self, event: &sse::Event) -> Result<Option<String>, AiError> {
        match parse(event)? {
            Some(v) => self.apply(&v),
            None => Ok(None),
        }
    }

    fn apply(&mut self, v: &Value) -> Result<Option<String>, AiError> {
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
        if choice["delta"]["refusal"]
            .as_str()
            .is_some_and(|r| !r.is_empty())
        {
            self.refused = true;
        }
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
            _ if self.refused => StopReason::Refusal { category: None },
            "stop" => StopReason::EndTurn,
            "length" => StopReason::MaxTokens,
            "tool_calls" | "function_call" => StopReason::ToolUse,
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

/// The chunk's JSON; `None` for the closing `[DONE]`.
fn parse(event: &sse::Event) -> Result<Option<Value>, AiError> {
    if event.data == "[DONE]" {
        return Ok(None);
    }
    serde_json::from_str(&event.data)
        .map(Some)
        .map_err(|e| AiError::Protocol(format!("bad chunk from provider: {e}")))
}

/// Collects a whole turn: its text and the tool calls whose pieces arrive
/// by index.
#[derive(Debug, Default)]
pub(crate) struct TurnDecoder {
    meta: Decoder,
    /// Id, name and arguments so far, by index.
    calls: Vec<(String, String, String)>,
}

impl TurnDecoder {
    pub fn handle(&mut self, event: &sse::Event) -> Result<Option<String>, AiError> {
        let Some(v) = parse(event)? else {
            return Ok(None);
        };
        for call in v["choices"][0]["delta"]["tool_calls"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let index = call["index"]
                .as_u64()
                .map_or(self.calls.len(), |i| i as usize);
            if self.calls.len() <= index {
                self.calls.resize(index + 1, Default::default());
            }
            let (id, name, arguments) = &mut self.calls[index];
            if let Some(more) = call["id"].as_str() {
                id.push_str(more);
            }
            if let Some(more) = call["function"]["name"].as_str() {
                name.push_str(more);
            }
            if let Some(more) = call["function"]["arguments"].as_str() {
                arguments.push_str(more);
            }
        }
        self.meta.apply(&v)
    }

    pub fn finish(self) -> Result<Reply, AiError> {
        let calls: Vec<_> = self
            .calls
            .into_iter()
            .filter(|(_, name, _)| !name.is_empty())
            .collect();
        let done = self.meta.finish()?;
        let stop_reason = match done.stop_reason {
            // Some servers say "stop" even when they called tools.
            StopReason::EndTurn | StopReason::Other(_) if !calls.is_empty() => StopReason::ToolUse,
            other => other,
        };
        let mut parts = Vec::new();
        if !matches!(stop_reason, StopReason::Refusal { .. }) {
            if !done.text.is_empty() {
                parts.push(Part::Text { text: done.text });
            }
            for (i, (id, name, arguments)) in calls.into_iter().enumerate() {
                parts.push(Part::ToolCall {
                    id: if id.is_empty() {
                        format!("call_{i}")
                    } else {
                        id
                    },
                    name,
                    input: tool_input(&arguments),
                });
            }
        }
        Ok(Reply {
            turn: Turn {
                role: Role::Assistant,
                parts,
            },
            stop_reason,
            usage: done.usage,
            model: done.model,
        })
    }
}

/// As in the Anthropic decoder: `{}` for nothing, the text for non-objects.
fn tool_input(raw: &str) -> Value {
    if raw.trim().is_empty() {
        return json!({});
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(value) if value.is_object() => value,
        _ => Value::String(raw.to_owned()),
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

    #[test]
    fn conversation_sends_tools_as_functions_and_results_as_tool_messages() {
        let request = ConversationRequest {
            system: Some("Help with mail.".into()),
            turns: vec![
                Turn::user("Any mail from Ana?"),
                Turn {
                    role: Role::Assistant,
                    parts: vec![
                        Part::Opaque {
                            provider: crate::ProviderKind::Anthropic,
                            block: json!({ "type": "thinking" }),
                        },
                        Part::ToolCall {
                            id: "call_1".into(),
                            name: "search_email".into(),
                            input: json!({ "query": "from:ana" }),
                        },
                    ],
                },
                Turn {
                    role: Role::User,
                    parts: vec![Part::ToolResult {
                        id: "call_1".into(),
                        content: "No such account.".into(),
                        is_error: true,
                    }],
                },
            ],
            tools: vec![crate::Tool {
                name: "search_email".into(),
                description: "Searches mail.".into(),
                input_schema: json!({ "type": "object" }),
            }],
            max_tokens: 16000,
        };
        let body = conversation_body(&config("http://localhost:11434/v1"), &request, false);
        assert_eq!(
            body["messages"],
            json!([
                { "role": "system", "content": "Help with mail." },
                { "role": "user", "content": "Any mail from Ana?" },
                { "role": "assistant", "content": null, "tool_calls": [
                    { "id": "call_1", "type": "function", "function": { "name": "search_email", "arguments": "{\"query\":\"from:ana\"}" } },
                ] },
                { "role": "tool", "tool_call_id": "call_1", "content": "Error: No such account." },
            ])
        );
        assert_eq!(body["tools"][0]["function"]["name"], "search_email");
        assert_eq!(
            body["tools"][0]["function"]["parameters"],
            json!({ "type": "object" })
        );
        assert_eq!(body["max_tokens"], 16000);
    }

    #[test]
    fn decodes_tool_calls_streamed_in_pieces() {
        let mut parser = sse::Parser::default();
        let mut decoder = TurnDecoder::default();
        let chunks = [
            json!({ "model": "m", "choices": [{ "delta": { "role": "assistant", "content": "Checking." } }] }),
            json!({ "choices": [{ "delta": { "tool_calls": [{ "index": 0, "id": "call_a", "function": { "name": "search_email", "arguments": "{\"qu" } }] } }] }),
            json!({ "choices": [{ "delta": { "tool_calls": [{ "index": 0, "function": { "arguments": "ery\":\"ana\"}" } }] } }] }),
            json!({ "choices": [{ "delta": { "tool_calls": [{ "index": 1, "id": "call_b", "function": { "name": "list_accounts", "arguments": "" } }] } }] }),
            // Some servers finish with "stop" anyway.
            json!({ "choices": [{ "delta": {}, "finish_reason": "stop" }] }),
        ];
        let stream: String = chunks
            .iter()
            .map(|c| format!("data: {c}\n\n"))
            .collect::<String>()
            + "data: [DONE]\n\n";
        let mut streamed = String::new();
        for event in parser.push(stream.as_bytes()) {
            if let Some(text) = decoder.handle(&event).unwrap() {
                streamed.push_str(&text);
            }
        }
        let reply = decoder.finish().unwrap();
        assert_eq!(streamed, "Checking.");
        assert_eq!(reply.stop_reason, StopReason::ToolUse);
        assert_eq!(
            reply.turn.parts,
            vec![
                Part::text("Checking."),
                Part::ToolCall {
                    id: "call_a".into(),
                    name: "search_email".into(),
                    input: json!({ "query": "ana" })
                },
                Part::ToolCall {
                    id: "call_b".into(),
                    name: "list_accounts".into(),
                    input: json!({})
                },
            ]
        );
    }

    #[test]
    fn a_refusal_in_words_is_a_refusal() {
        let mut parser = sse::Parser::default();
        let mut decoder = TurnDecoder::default();
        let stream = concat!(
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":null,\"refusal\":\"\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"refusal\":\"I can't help with that.\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        for event in parser.push(stream.as_bytes()) {
            assert_eq!(decoder.handle(&event).unwrap(), None);
        }
        let reply = decoder.finish().unwrap();
        assert_eq!(reply.stop_reason, StopReason::Refusal { category: None });
        assert!(reply.turn.parts.is_empty());
    }
}
