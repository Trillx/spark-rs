//! The events a streamed answer (a summary, a drafted reply) sends to the UI.

use serde::Serialize;
use spark_ai::{Completion, StopReason};

/// What the UI receives over its channel: any number of deltas, then exactly
/// one of the other events.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "event",
    content = "data"
)]
pub enum AiEvent {
    Delta {
        text: String,
    },
    Done {
        model: String,
        input_tokens: u64,
        output_tokens: u64,
        /// Stopped at the token limit, so the answer may end mid-sentence.
        truncated: bool,
    },
    /// The provider declined; the UI must discard the text it streamed.
    Refused {
        category: Option<String>,
    },
    Error {
        message: String,
    },
    /// No provider, key, URL or model yet.
    NotConfigured {
        message: String,
    },
}

impl AiEvent {
    /// The final event of a run. `result` carries an error already in words
    /// for the UI.
    pub fn finished(result: Result<Completion, String>, requested_model: &str) -> Self {
        let completion = match result {
            Ok(completion) => completion,
            Err(message) => return AiEvent::Error { message },
        };
        match completion.stop_reason {
            StopReason::Refusal { category } => AiEvent::Refused { category },
            _ if completion.text.trim().is_empty() => AiEvent::Error {
                message: "The provider returned an empty answer.".into(),
            },
            reason => AiEvent::Done {
                model: completion
                    .model
                    .unwrap_or_else(|| requested_model.to_owned()),
                input_tokens: completion.usage.input_tokens,
                output_tokens: completion.usage.output_tokens,
                truncated: reason == StopReason::MaxTokens,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use spark_ai::Usage;

    fn completion(text: &str, stop_reason: StopReason, model: Option<&str>) -> Completion {
        Completion {
            text: text.into(),
            stop_reason,
            usage: Usage {
                input_tokens: 812,
                output_tokens: 64,
            },
            model: model.map(str::to_owned),
        }
    }

    #[test]
    fn done_reports_the_answering_model_and_usage() {
        let event = AiEvent::finished(
            Ok(completion(
                "Dana needs numbers.",
                StopReason::EndTurn,
                Some("claude-sonnet-5-5"),
            )),
            "claude-opus-5-5",
        );
        assert_eq!(
            event,
            AiEvent::Done {
                model: "claude-sonnet-5-5".into(),
                input_tokens: 812,
                output_tokens: 64,
                truncated: false,
            }
        );
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "event": "done",
                "data": { "model": "claude-sonnet-5-5", "inputTokens": 812, "outputTokens": 64, "truncated": false },
            })
        );

        let fallback = AiEvent::finished(
            Ok(completion("Cut", StopReason::MaxTokens, None)),
            "gpt-5.5",
        );
        assert!(matches!(
            fallback,
            AiEvent::Done { ref model, truncated: true, .. } if model == "gpt-5.5"
        ));
    }

    #[test]
    fn refusals_errors_and_empty_answers() {
        let refused = AiEvent::finished(
            Ok(completion(
                "",
                StopReason::Refusal {
                    category: Some("cyber".into()),
                },
                None,
            )),
            "m",
        );
        assert_eq!(
            serde_json::to_value(&refused).unwrap(),
            json!({ "event": "refused", "data": { "category": "cyber" } })
        );
        assert_eq!(
            AiEvent::finished(Err("Couldn't reach the provider".into()), "m"),
            AiEvent::Error {
                message: "Couldn't reach the provider".into()
            }
        );
        assert!(matches!(
            AiEvent::finished(Ok(completion("  \n", StopReason::EndTurn, None)), "m"),
            AiEvent::Error { .. }
        ));
        assert_eq!(
            serde_json::to_value(AiEvent::NotConfigured {
                message: "x".into()
            })
            .unwrap(),
            json!({ "event": "notConfigured", "data": { "message": "x" } })
        );
        assert_eq!(
            serde_json::to_value(AiEvent::Delta { text: "Hi".into() }).unwrap(),
            json!({ "event": "delta", "data": { "text": "Hi" } })
        );
    }
}
