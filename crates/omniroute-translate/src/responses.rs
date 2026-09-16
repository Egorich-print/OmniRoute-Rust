//! Streaming `openai-responses → openai` conversion.
//!
//! Mirrors `openaiResponsesToOpenAIResponseStream` in
//! `open-sse/translator/response/openai-responses.ts` for the text path:
//! `response.output_text.delta` becomes a content delta,
//! `response.reasoning_*_text.delta` becomes `reasoning_content`, and the
//! first `response.completed` becomes the terminal finish chunk with usage.
//! Tool-call synthesis and namespaces are follow-ups, not silent behaviour
//! changes: unknown event kinds are ignored (`Ignored`), never fabricated.

use serde_json::{Value, json};

/// Mutable per-stream conversion state (the JS translator's `state`).
#[derive(Debug, Clone)]
pub struct ResponsesState {
    /// `chatcmpl-*` id stamped on every emitted chunk.
    pub chat_id: String,
    /// `created` timestamp stamped on every emitted chunk.
    pub created: i64,
    /// Model name stamped on every emitted chunk.
    pub model: String,
    /// Whether the `role: "assistant"` announcement already went out.
    pub role_sent: bool,
    /// Whether the terminal finish chunk already went out.
    pub finish_sent: bool,
}

impl ResponsesState {
    /// Fresh state for one stream.
    pub fn new(chat_id: String, created: i64, model: String) -> Self {
        Self {
            chat_id,
            created,
            model,
            role_sent: false,
            finish_sent: false,
        }
    }
}

/// Token accounting extracted from a `response.completed` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TranslatedUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cached_tokens: u32,
    pub cache_creation_tokens: u32,
    pub reasoning_tokens: u32,
}

/// Outcome of converting one upstream event.
#[derive(Debug, Clone, PartialEq)]
pub enum Converted {
    /// A `chat.completion.chunk` object to emit to the client.
    Emit(Value),
    /// The event carries nothing for an OpenAI client.
    Ignore,
    /// The upstream failed; carries the human-readable message.
    Fail(String),
}

/// Convert one Responses API streaming event to an OpenAI chunk.
///
/// `event_type` is the SSE `event:` name (equivalently the payload `type`);
/// `data` is the event payload.
pub fn convert_event(state: &mut ResponsesState, event_type: &str, data: &Value) -> Converted {
    match event_type {
        "response.output_text.delta" => {
            let text = data.get("delta").and_then(Value::as_str).unwrap_or("");
            if text.is_empty() {
                return Converted::Ignore;
            }
            Converted::Emit(delta_chunk(state, Some(text), None))
        }
        "response.reasoning_summary_text.delta"
        | "response.reasoning_content_text.delta"
        | "response.reasoning_text.delta" => {
            let text = data.get("delta").and_then(Value::as_str).unwrap_or("");
            if text.is_empty() {
                return Converted::Ignore;
            }
            Converted::Emit(delta_chunk(state, None, Some(text)))
        }
        "response.output_text.done" => Converted::Ignore,
        "response.completed" => {
            if state.finish_sent {
                return Converted::Ignore;
            }
            state.finish_sent = true;
            let usage = extract_usage(data);
            Converted::Emit(finish_chunk(state, "stop", usage))
        }
        "response.failed" | "error" => {
            state.finish_sent = true;
            Converted::Fail(error_message(data))
        }
        // response.created / in_progress, output_item.*, content_part.* and
        // anything else carry nothing for a Chat client on their own.
        _ => Converted::Ignore,
    }
}

fn delta_chunk(
    state: &mut ResponsesState,
    content: Option<&str>,
    reasoning: Option<&str>,
) -> Value {
    let mut delta = serde_json::Map::new();
    if !state.role_sent {
        delta.insert("role".to_string(), Value::from("assistant"));
        state.role_sent = true;
    }
    if let Some(text) = content {
        delta.insert("content".to_string(), Value::from(text));
    }
    if let Some(text) = reasoning {
        delta.insert("reasoning_content".to_string(), Value::from(text));
    }
    json!({
        "id": state.chat_id,
        "object": "chat.completion.chunk",
        "created": state.created,
        "model": state.model,
        "choices": [{
            "index": 0,
            "delta": Value::Object(delta),
            "finish_reason": null,
        }],
    })
}

fn finish_chunk(state: &ResponsesState, reason: &str, usage: Option<TranslatedUsage>) -> Value {
    let mut chunk = json!({
        "id": state.chat_id,
        "object": "chat.completion.chunk",
        "created": state.created,
        "model": state.model,
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": reason,
        }],
    });
    if let Some(usage) = usage {
        chunk
            .as_object_mut()
            .expect("chunk is an object")
            .insert("usage".to_string(), usage_json(&usage));
    }
    chunk
}

fn usage_json(usage: &TranslatedUsage) -> Value {
    let mut root = serde_json::Map::new();
    root.insert("prompt_tokens".to_string(), json!(usage.prompt_tokens));
    root.insert(
        "completion_tokens".to_string(),
        json!(usage.completion_tokens),
    );
    root.insert(
        "total_tokens".to_string(),
        json!(usage.prompt_tokens + usage.completion_tokens),
    );
    if usage.cached_tokens > 0 || usage.cache_creation_tokens > 0 {
        let mut details = serde_json::Map::new();
        if usage.cached_tokens > 0 {
            details.insert("cached_tokens".to_string(), json!(usage.cached_tokens));
        }
        if usage.cache_creation_tokens > 0 {
            details.insert(
                "cache_creation_tokens".to_string(),
                json!(usage.cache_creation_tokens),
            );
        }
        root.insert("prompt_tokens_details".to_string(), Value::Object(details));
    }
    if usage.reasoning_tokens > 0 {
        root.insert(
            "completion_tokens_details".to_string(),
            json!({ "reasoning_tokens": usage.reasoning_tokens }),
        );
    }
    Value::Object(root)
}

fn u64_at(value: &Value, path: &[&str]) -> Option<u64> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_u64()
}

fn first_nonzero(candidates: &[Option<u64>]) -> u32 {
    candidates
        .iter()
        .filter_map(|candidate| *candidate)
        .find(|value| *value != 0)
        .unwrap_or(0) as u32
}

/// Extract usage from a `response.completed` payload, mirroring the JS
/// field fallbacks (`input_tokens || prompt_tokens`, the
/// `*_tokens_details` variants, and the `cache_read_input_tokens` prompt
/// adjustment).
fn extract_usage(data: &Value) -> Option<TranslatedUsage> {
    let response = data.get("response")?.get("usage")?;
    if !response.is_object() {
        return None;
    }

    let input = first_nonzero(&[
        u64_at(response, &["input_tokens"]),
        u64_at(response, &["prompt_tokens"]),
    ]);
    let output = first_nonzero(&[
        u64_at(response, &["output_tokens"]),
        u64_at(response, &["completion_tokens"]),
    ]);
    let cached = first_nonzero(&[
        u64_at(response, &["cache_read_input_tokens"]),
        u64_at(response, &["input_tokens_details", "cached_tokens"]),
        u64_at(response, &["prompt_tokens_details", "cached_tokens"]),
    ]);
    let cache_creation = first_nonzero(&[u64_at(response, &["cache_creation_input_tokens"])]);
    let reasoning = first_nonzero(&[
        u64_at(response, &["output_tokens_details", "reasoning_tokens"]),
        u64_at(response, &["completion_tokens_details", "reasoning_tokens"]),
        u64_at(response, &["reasoning_tokens"]),
    ]);

    let prompt = input
        + if response.get("cache_read_input_tokens").is_some() {
            cached + cache_creation
        } else {
            0
        };

    Some(TranslatedUsage {
        prompt_tokens: prompt,
        completion_tokens: output,
        cached_tokens: cached,
        cache_creation_tokens: cache_creation,
        reasoning_tokens: reasoning,
    })
}

fn error_message(data: &Value) -> String {
    data.get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .or_else(|| data.get("message").and_then(Value::as_str))
        .unwrap_or("upstream error")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn state() -> ResponsesState {
        ResponsesState::new(
            "chatcmpl-test-001".to_string(),
            1700000000,
            "grok-4.6".to_string(),
        )
    }

    #[test]
    fn empty_delta_is_ignored() {
        let mut state = state();
        assert_eq!(
            convert_event(
                &mut state,
                "response.output_text.delta",
                &json!({"delta": ""})
            ),
            Converted::Ignore
        );
    }

    #[test]
    fn role_announced_once_on_first_delta() {
        let mut state = state();
        let first = convert_event(
            &mut state,
            "response.output_text.delta",
            &json!({"delta": "Hi"}),
        );
        let second = convert_event(
            &mut state,
            "response.output_text.delta",
            &json!({"delta": "!"}),
        );
        match (&first, &second) {
            (Converted::Emit(a), Converted::Emit(b)) => {
                assert_eq!(a["choices"][0]["delta"]["role"], json!("assistant"));
                assert!(b["choices"][0]["delta"].get("role").is_none());
            }
            _ => panic!("expected two chunks, got {first:?} and {second:?}"),
        }
    }

    #[test]
    fn second_completed_is_ignored() {
        let mut state = state();
        let usage = json!({"response": {"usage": {"input_tokens": 1, "output_tokens": 1}}});
        assert!(matches!(
            convert_event(&mut state, "response.completed", &usage),
            Converted::Emit(_)
        ));
        assert_eq!(
            convert_event(&mut state, "response.completed", &usage),
            Converted::Ignore
        );
    }

    #[test]
    fn failed_event_reports_message() {
        let mut state = state();
        assert_eq!(
            convert_event(
                &mut state,
                "response.failed",
                &json!({"error": {"message": "boom"}}),
            ),
            Converted::Fail("boom".to_string())
        );
    }
}
