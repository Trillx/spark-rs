//! Anthropic's Messages API (`POST /v1/messages`, streamed).

use serde_json::{Value, json};

use crate::{
    AiError, ChatRequest, Completion, ConversationRequest, Part, ProviderConfig, ProviderKind,
    Reply, Role, StopReason, Turn, Usage, sse,
};

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

pub(crate) fn conversation_request(
    http: &reqwest::Client,
    config: &ProviderConfig,
    api_key: Option<&str>,
    request: &ConversationRequest,
) -> reqwest::RequestBuilder {
    let base = config.base_url.trim_end_matches('/');
    let official = base == DEFAULT_BASE_URL;
    let fallback = official && supports_default_fallback(&config.model);
    let mut builder = http
        .post(format!("{base}/v1/messages"))
        .header("anthropic-version", API_VERSION)
        .json(&conversation_body(config, request, official, fallback));
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

/// `official`: the request goes to Anthropic's own API, which takes
/// `eager_input_streaming`; proxies may reject it.
fn conversation_body(
    config: &ProviderConfig,
    request: &ConversationRequest,
    official: bool,
    fallback: bool,
) -> Value {
    let chat = ChatRequest {
        system: request.system.clone(),
        messages: Vec::new(),
        max_tokens: request.max_tokens,
    };
    let mut body = body(config, &chat, fallback);
    body["messages"] = request.turns.iter().map(turn).collect();
    if !request.tools.is_empty() {
        body["tools"] = request
            .tools
            .iter()
            .map(|t| {
                let mut tool = json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                });
                if official {
                    tool["eager_input_streaming"] = json!(true);
                }
                tool
            })
            .collect();
    }
    body
}

fn turn(turn: &Turn) -> Value {
    let content: Vec<Value> = turn
        .parts
        .iter()
        .filter_map(|part| match part {
            // The API rejects empty text blocks.
            Part::Text { text } if text.is_empty() => None,
            Part::Text { text } => Some(json!({ "type": "text", "text": text })),
            Part::ToolCall { id, name, input } => Some(json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": if input.is_object() { input.clone() } else { json!({}) },
            })),
            Part::ToolResult {
                id,
                content,
                is_error,
            } => Some(json!({
                "type": "tool_result",
                "tool_use_id": id,
                "content": content,
                "is_error": is_error,
            })),
            Part::Opaque {
                provider: ProviderKind::Anthropic,
                block,
            } => Some(block.clone()),
            Part::Opaque { .. } => None,
        })
        .collect();
    json!({ "role": turn.role, "content": content })
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
        self.apply(&parse(event)?)
    }

    fn apply(&mut self, v: &Value) -> Result<Option<String>, AiError> {
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

/// Collects a whole turn: its text, tool calls and the blocks that have to
/// go back unchanged (thinking).
#[derive(Debug, Default)]
pub(crate) struct TurnDecoder {
    meta: Decoder,
    /// By index: the block as it started, with deltas applied, and a
    /// tool call's input JSON so far.
    blocks: Vec<Option<(Value, String)>>,
}

impl TurnDecoder {
    /// Returns the text this event added, if any.
    pub fn handle(&mut self, event: &sse::Event) -> Result<Option<String>, AiError> {
        let v = parse(event)?;
        let index = v["index"].as_u64().map(|i| i as usize);
        match (v["type"].as_str(), index) {
            (Some("content_block_start"), Some(i)) => {
                if self.blocks.len() <= i {
                    self.blocks.resize(i + 1, None);
                }
                self.blocks[i] = Some((v["content_block"].clone(), String::new()));
            }
            (Some("content_block_delta"), Some(i)) => {
                if let Some(Some((block, input))) = self.blocks.get_mut(i) {
                    let delta = &v["delta"];
                    let append = |block: &mut Value, field: &str, key: &str| {
                        let more = delta[key].as_str().unwrap_or_default();
                        let so_far = block[field].as_str().unwrap_or_default();
                        block[field] = json!(format!("{so_far}{more}"));
                    };
                    match delta["type"].as_str() {
                        Some("text_delta") => append(block, "text", "text"),
                        Some("thinking_delta") => append(block, "thinking", "thinking"),
                        Some("signature_delta") => block["signature"] = delta["signature"].clone(),
                        Some("input_json_delta") => {
                            input.push_str(delta["partial_json"].as_str().unwrap_or_default())
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        self.meta.apply(&v)
    }

    pub fn finish(self) -> Result<Reply, AiError> {
        let done = self.meta.finish()?;
        let refused = matches!(done.stop_reason, StopReason::Refusal { .. });
        let blocks: Vec<(Value, String)> = self.blocks.into_iter().flatten().collect();
        // After a fallback mid-answer, what the declined model started other
        // than text doesn't go back: its thinking and unfinished tool calls.
        let boundary = blocks.iter().rposition(|(b, _)| b["type"] == "fallback");
        let parts = if refused {
            Vec::new()
        } else {
            blocks
                .into_iter()
                .enumerate()
                .filter_map(|(i, (block, input))| {
                    let kind = block["type"].as_str().unwrap_or_default().to_owned();
                    if boundary.is_some_and(|b| i <= b) && kind != "text" {
                        return None;
                    }
                    match kind.as_str() {
                        "text" => Some(Part::Text {
                            text: block["text"].as_str().unwrap_or_default().to_owned(),
                        }),
                        "tool_use" => Some(Part::ToolCall {
                            id: block["id"].as_str().unwrap_or_default().to_owned(),
                            name: block["name"].as_str().unwrap_or_default().to_owned(),
                            input: tool_input(&input),
                        }),
                        _ => Some(Part::Opaque {
                            provider: ProviderKind::Anthropic,
                            block,
                        }),
                    }
                })
                .collect()
        };
        Ok(Reply {
            turn: Turn {
                role: Role::Assistant,
                parts,
            },
            stop_reason: done.stop_reason,
            usage: done.usage,
            model: done.model,
        })
    }
}

fn parse(event: &sse::Event) -> Result<Value, AiError> {
    serde_json::from_str(&event.data)
        .map_err(|e| AiError::Protocol(format!("bad event from Anthropic: {e}")))
}

/// A tool call's streamed input: no input is `{}`, and input that isn't a
/// JSON object stays the text it was, to be reported back.
fn tool_input(raw: &str) -> Value {
    if raw.trim().is_empty() {
        return json!({});
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(value) if value.is_object() => value,
        _ => Value::String(raw.to_owned()),
    }
}

fn stop_reason(reason: &str, details: &Value) -> StopReason {
    match reason {
        "end_turn" | "stop_sequence" => StopReason::EndTurn,
        "max_tokens" => StopReason::MaxTokens,
        "tool_use" => StopReason::ToolUse,
        "refusal" => StopReason::Refusal {
            category: details["category"].as_str().map(str::to_owned),
        },
        other => StopReason::Other(other.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Effort, Message};

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

    fn conversation() -> ConversationRequest {
        let thinking = json!({ "type": "thinking", "thinking": "", "signature": "sig" });
        ConversationRequest {
            system: Some("Help with mail.".into()),
            turns: vec![
                Turn::user("Any mail from Ana?"),
                Turn {
                    role: Role::Assistant,
                    parts: vec![
                        Part::Opaque {
                            provider: ProviderKind::Anthropic,
                            block: thinking.clone(),
                        },
                        Part::Opaque {
                            provider: ProviderKind::OpenAiCompatible,
                            block: json!({ "x": 1 }),
                        },
                        Part::text(""),
                        Part::ToolCall {
                            id: "toolu_1".into(),
                            name: "search_email".into(),
                            input: json!({ "query": "from:ana" }),
                        },
                    ],
                },
                Turn {
                    role: Role::User,
                    parts: vec![Part::ToolResult {
                        id: "toolu_1".into(),
                        content: "[]".into(),
                        is_error: false,
                    }],
                },
            ],
            tools: vec![crate::Tool {
                name: "search_email".into(),
                description: "Searches mail.".into(),
                input_schema: json!({ "type": "object", "properties": {} }),
            }],
            max_tokens: 16000,
        }
    }

    #[test]
    fn conversation_passes_thinking_back_and_streams_tool_input_only_on_the_api() {
        let request = conversation();
        let official = conversation_body(
            &config(DEFAULT_BASE_URL, "claude-opus-5-5"),
            &request,
            true,
            true,
        );
        let assistant = &official["messages"][1]["content"];
        assert_eq!(
            assistant,
            &json!([
                { "type": "thinking", "thinking": "", "signature": "sig" },
                { "type": "tool_use", "id": "toolu_1", "name": "search_email", "input": { "query": "from:ana" } },
            ])
        );
        assert_eq!(
            official["messages"][2]["content"][0],
            json!({ "type": "tool_result", "tool_use_id": "toolu_1", "content": "[]", "is_error": false })
        );
        assert_eq!(official["tools"][0]["eager_input_streaming"], true);
        assert_eq!(official["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(official["fallbacks"], "default");
        assert_eq!(official["system"], "Help with mail.");

        let proxy = conversation_body(
            &config("https://gateway.example", "claude-opus-5-5"),
            &request,
            false,
            false,
        );
        assert!(proxy["tools"][0].get("eager_input_streaming").is_none());
        assert!(proxy.get("fallbacks").is_none());
    }

    fn decode_turn(stream: &str) -> (String, Reply) {
        let mut parser = sse::Parser::default();
        let mut decoder = TurnDecoder::default();
        let mut streamed = String::new();
        for event in parser.push(stream.as_bytes()) {
            if let Some(text) = decoder.handle(&event).unwrap() {
                streamed.push_str(&text);
            }
        }
        (streamed, decoder.finish().unwrap())
    }

    fn event(data: Value) -> String {
        format!("data: {data}\n\n")
    }

    #[test]
    fn decodes_thinking_text_and_tool_calls_into_a_turn() {
        let stream = [
            json!({ "type": "message_start", "message": { "model": "claude-opus-5-5", "usage": { "input_tokens": 900 } } }),
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "thinking", "thinking": "" } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "thinking_delta", "thinking": "Search " } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "thinking_delta", "thinking": "first." } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "signature_delta", "signature": "abc" } }),
            json!({ "type": "content_block_stop", "index": 0 }),
            json!({ "type": "content_block_start", "index": 1, "content_block": { "type": "text", "text": "" } }),
            json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "text_delta", "text": "Let me look." } }),
            json!({ "type": "content_block_start", "index": 2, "content_block": { "type": "tool_use", "id": "toolu_9", "name": "search_email", "input": {} } }),
            json!({ "type": "content_block_delta", "index": 2, "delta": { "type": "input_json_delta", "partial_json": "{\"query\": " } }),
            json!({ "type": "content_block_delta", "index": 2, "delta": { "type": "input_json_delta", "partial_json": "\"invoice\"}" } }),
            json!({ "type": "content_block_start", "index": 3, "content_block": { "type": "tool_use", "id": "toolu_10", "name": "list_accounts", "input": {} } }),
            json!({ "type": "content_block_start", "index": 4, "content_block": { "type": "tool_use", "id": "toolu_11", "name": "read_email", "input": {} } }),
            json!({ "type": "content_block_delta", "index": 4, "delta": { "type": "input_json_delta", "partial_json": "{\"id\": 4" } }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" }, "usage": { "output_tokens": 80 } }),
        ]
        .map(event)
        .concat();
        let (streamed, reply) = decode_turn(&stream);
        assert_eq!(streamed, "Let me look.");
        assert_eq!(reply.stop_reason, StopReason::ToolUse);
        assert_eq!(reply.usage.input_tokens, 900);
        assert_eq!(
            reply.turn.parts,
            vec![
                Part::Opaque {
                    provider: ProviderKind::Anthropic,
                    block: json!({ "type": "thinking", "thinking": "Search first.", "signature": "abc" }),
                },
                Part::text("Let me look."),
                Part::ToolCall {
                    id: "toolu_9".into(),
                    name: "search_email".into(),
                    input: json!({ "query": "invoice" })
                },
                Part::ToolCall {
                    id: "toolu_10".into(),
                    name: "list_accounts".into(),
                    input: json!({})
                },
                // Cut off: reported back rather than run.
                Part::ToolCall {
                    id: "toolu_11".into(),
                    name: "read_email".into(),
                    input: json!("{\"id\": 4")
                },
            ]
        );
        let bad: Vec<_> = reply
            .turn
            .tool_calls()
            .filter_map(|(id, _, input)| Part::invalid_input(id, input))
            .collect();
        assert_eq!(
            bad,
            vec![Part::ToolResult {
                id: "toolu_11".into(),
                content: json!({ "INVALID_JSON": "{\"id\": 4" }).to_string(),
                is_error: true,
            }]
        );
    }

    #[test]
    fn after_a_fallback_only_text_from_before_it_goes_back() {
        let stream = [
            json!({ "type": "message_start", "message": { "model": "claude-opus-5-5", "usage": { "input_tokens": 5 } } }),
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "thinking", "thinking": "", "signature": "s1" } }),
            json!({ "type": "content_block_start", "index": 1, "content_block": { "type": "text", "text": "Part" } }),
            json!({ "type": "content_block_start", "index": 2, "content_block": { "type": "tool_use", "id": "toolu_1", "name": "search_email", "input": {} } }),
            json!({ "type": "content_block_start", "index": 3, "content_block": { "type": "fallback", "from": { "model": "claude-opus-5-5" }, "to": { "model": "claude-opus-5" } } }),
            json!({ "type": "content_block_start", "index": 4, "content_block": { "type": "thinking", "thinking": "", "signature": "s2" } }),
            json!({ "type": "content_block_start", "index": 5, "content_block": { "type": "text", "text": "" } }),
            json!({ "type": "content_block_delta", "index": 5, "delta": { "type": "text_delta", "text": "ial answer." } }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 9 } }),
        ]
        .map(event)
        .concat();
        let (_, reply) = decode_turn(&stream);
        assert_eq!(
            reply.turn.parts,
            vec![
                Part::text("Part"),
                Part::Opaque {
                    provider: ProviderKind::Anthropic,
                    block: json!({ "type": "thinking", "thinking": "", "signature": "s2" }),
                },
                Part::text("ial answer."),
            ]
        );
        assert_eq!(reply.turn.text(), "Partial answer.");
    }

    #[test]
    fn a_refused_turn_has_no_parts() {
        let stream = [
            json!({ "type": "message_start", "message": { "model": "claude-opus-5-5", "usage": { "input_tokens": 5 } } }),
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "tool_use", "id": "toolu_1", "name": "search_email", "input": {} } }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "refusal", "stop_details": { "category": "cyber" } }, "usage": { "output_tokens": 1 } }),
        ]
        .map(event)
        .concat();
        let (_, reply) = decode_turn(&stream);
        assert!(reply.turn.parts.is_empty());
        assert_eq!(
            reply.stop_reason,
            StopReason::Refusal {
                category: Some("cyber".into())
            }
        );
    }
}
