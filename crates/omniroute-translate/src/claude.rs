//! Streaming `claude → openai` conversion (text path).
//!
//! Mirrors `claudeToOpenAIResponse` in
//! `open-sse/translator/response/claude-to-openai.ts` for plain text:
//! `message_start` announces the role, `content_block_delta` text/thinking
//! deltas become content/`reasoning_content`, and `message_delta` usage plus
//! `message_stop` close the stream with a finish chunk.
//!
//! Explicitly deferred: tool_use/input_json synthesis, `<think>` tag parsing,
//! `pendingThinkClose` markers, and signature handling. Unknown event kinds
//! are ignored, never fabricated.

use serde_json::{Value, json};

/// Mutable per-stream conversion state.
#[derive(Debug, Clone, Default)]
pub struct ClaudeState {
    /// Claude `message.id`, stamped as `chatcmpl-{id}`.
    pub message_id: Option<String>,
    /// Model name from `message_start`.
    pub model: Option<String>,
    /// Whether the terminal finish chunk already went out.
    pub finish_sent: bool,
    /// Finish reason captured from a `stop_reason`, if any.
    pub finish_reason: Option<String>,
    /// Accumulated token accounting.
    pub usage: Option<ClaudeUsage>,
}

/// Token accounting in Claude-native fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClaudeUsage {
    pub input: u32,
    pub output: u32,
    pub cache_read: u32,
    pub cache_creation: u32,
    pub reasoning: Option<u32>,
}

/// Outcome of converting one upstream event.
#[derive(Debug, Clone, PartialEq)]
pub enum Converted {
    /// A `chat.completion.chunk` object to emit to the client.
    Emit(Value),
    /// The event carries nothing for an OpenAI client on its own.
    Ignore,
}

/// Convert one Claude SSE event to an OpenAI chunk.
///
/// `event_type` is the SSE `type` field, `data` the full event object, and
/// `now` the Unix timestamp stamped as `created` (injected for deterministic
/// tests; production passes wall time).
pub fn convert_event(
    state: &mut ClaudeState,
    event_type: &str,
    data: &Value,
    now: i64,
) -> Converted {
    match event_type {
        "message_start" => {
            let message = data.get("message");
            state.message_id = message
                .and_then(|message| message.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| Some(format!("msg_{now}")));
            state.model = message
                .and_then(|message| message.get("model"))
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Some(usage) = message.and_then(|message| message.get("usage")) {
                let input = first_number(usage, &["input_tokens", "prompt_tokens"]);
                let output = first_number(usage, &["output_tokens", "completion_tokens"]);
                let cache_read = first_number(usage, &["cache_read_input_tokens"]);
                let cache_creation = first_number(usage, &["cache_creation_input_tokens"]);
                if input > 0 || output > 0 || cache_read > 0 || cache_creation > 0 {
                    state.usage = Some(ClaudeUsage {
                        input,
                        output,
                        cache_read,
                        cache_creation,
                        reasoning: None,
                    });
                }
            }
            Converted::Emit(role_chunk(state, now))
        }
        "content_block_start" | "content_block_stop" => Converted::Ignore,
        "content_block_delta" => {
            let delta = data.get("delta");
            let kind = delta
                .and_then(|delta| delta.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("");
            match kind {
                "text_delta" => {
                    let text = delta
                        .and_then(|delta| delta.get("text"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if text.is_empty() {
                        return Converted::Ignore;
                    }
                    Converted::Emit(delta_chunk(state, now, Some(text), None))
                }
                "thinking_delta" => {
                    let text = delta
                        .and_then(|delta| delta.get("thinking"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if text.is_empty() {
                        return Converted::Ignore;
                    }
                    Converted::Emit(delta_chunk(state, now, None, Some(text)))
                }
                _ => Converted::Ignore,
            }
        }
        "message_delta" => {
            if let Some(usage) = data.get("usage") {
                merge_usage(state, usage);
            }
            let stop_reason = data
                .get("delta")
                .and_then(|delta| delta.get("stop_reason"))
                .and_then(Value::as_str);
            match stop_reason {
                Some(reason) => {
                    state.finish_reason = Some(convert_stop_reason(reason));
                    state.finish_sent = true;
                    Converted::Emit(finish_chunk(state, now))
                }
                None => Converted::Ignore,
            }
        }
        "message_stop" => {
            if state.finish_sent {
                return Converted::Ignore;
            }
            state.finish_sent = true;
            Converted::Emit(finish_chunk(state, now))
        }
        _ => Converted::Ignore,
    }
}

/// Map a Claude `stop_reason` to an OpenAI `finish_reason`.
fn convert_stop_reason(reason: &str) -> String {
    match reason {
        "end_turn" | "stop_sequence" => "stop",
        "max_tokens" => "length",
        "tool_use" => "tool_calls",
        _ => "stop",
    }
    .to_string()
}

fn first_number(value: &Value, keys: &[&str]) -> u32 {
    keys.iter()
        .filter_map(|key| value.get(*key))
        .filter_map(Value::as_u64)
        .find(|value| *value != 0)
        .unwrap_or(0) as u32
}

/// Merge a `message_delta` usage block into the accumulated accounting,
/// mirroring the JS billable-input rule (new input wins when present,
/// otherwise the previous total stands; cache fields persist).
fn merge_usage(state: &mut ClaudeState, usage: &Value) {
    let previous = state.usage.unwrap_or_default();
    let input = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let output = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let cache_read = usage
        .get("cache_read_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let cache_creation = usage
        .get("cache_creation_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let reasoning = usage
        .get("output_tokens_details")
        .and_then(|details| details.get("thinking_tokens"))
        .and_then(Value::as_u64)
        .map(|value| value as u32);

    let billable = if input > 0 || cache_read > 0 || cache_creation > 0 {
        input + cache_read
    } else {
        previous.input
    };
    state.usage = Some(ClaudeUsage {
        input: billable,
        output,
        cache_read: if cache_read > 0 {
            cache_read
        } else {
            previous.cache_read
        },
        cache_creation: if cache_creation > 0 {
            cache_creation
        } else {
            previous.cache_creation
        },
        reasoning: reasoning.or(previous.reasoning),
    });
}

fn chunk_id(state: &ClaudeState) -> String {
    format!(
        "chatcmpl-{}",
        state.message_id.as_deref().unwrap_or("unknown")
    )
}

fn chunk_model(state: &ClaudeState) -> String {
    state.model.clone().unwrap_or_else(|| "unknown".to_string())
}

fn role_chunk(state: &ClaudeState, now: i64) -> Value {
    json!({
        "id": chunk_id(state),
        "object": "chat.completion.chunk",
        "created": now,
        "model": chunk_model(state),
        "choices": [{
            "index": 0,
            "delta": {"role": "assistant"},
            "finish_reason": null,
        }],
    })
}

fn delta_chunk(
    state: &ClaudeState,
    now: i64,
    content: Option<&str>,
    reasoning: Option<&str>,
) -> Value {
    let mut delta = serde_json::Map::new();
    if let Some(text) = content {
        delta.insert("content".to_string(), Value::from(text));
    }
    if let Some(text) = reasoning {
        delta.insert("reasoning_content".to_string(), Value::from(text));
    }
    json!({
        "id": chunk_id(state),
        "object": "chat.completion.chunk",
        "created": now,
        "model": chunk_model(state),
        "choices": [{
            "index": 0,
            "delta": Value::Object(delta),
            "finish_reason": null,
        }],
    })
}

fn finish_chunk(state: &ClaudeState, now: i64) -> Value {
    let reason = state.finish_reason.clone().unwrap_or_else(|| {
        // No tool tracking yet, so a bare stop always means "stop".
        "stop".to_string()
    });
    let mut chunk = json!({
        "id": chunk_id(state),
        "object": "chat.completion.chunk",
        "created": now,
        "model": chunk_model(state),
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": reason,
        }],
    });
    if let Some(usage) = state.usage {
        let mut root = serde_json::Map::new();
        root.insert("prompt_tokens".to_string(), json!(usage.input));
        root.insert("completion_tokens".to_string(), json!(usage.output));
        root.insert(
            "total_tokens".to_string(),
            json!(usage.input + usage.output),
        );
        if let Some(reasoning) = usage.reasoning {
            root.insert("reasoning_tokens".to_string(), json!(reasoning));
            root.insert(
                "completion_tokens_details".to_string(),
                json!({ "reasoning_tokens": reasoning }),
            );
        }
        if usage.cache_read > 0 || usage.cache_creation > 0 {
            let mut details = serde_json::Map::new();
            if usage.cache_read > 0 {
                details.insert("cached_tokens".to_string(), json!(usage.cache_read));
            }
            if usage.cache_creation > 0 {
                details.insert(
                    "cache_creation_tokens".to_string(),
                    json!(usage.cache_creation),
                );
            }
            root.insert("prompt_tokens_details".to_string(), Value::Object(details));
        }
        chunk
            .as_object_mut()
            .expect("chunk is an object")
            .insert("usage".to_string(), Value::Object(root));
    }
    chunk
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1700000000;

    fn started() -> ClaudeState {
        let mut state = ClaudeState::default();
        convert_event(
            &mut state,
            "message_start",
            &json!({"message": {"id": "msg_1", "model": "m"}}),
            NOW,
        );
        state
    }

    #[test]
    fn empty_text_delta_is_ignored() {
        let mut state = started();
        assert_eq!(
            convert_event(
                &mut state,
                "content_block_delta",
                &json!({"delta": {"type": "text_delta", "text": ""}}),
                NOW,
            ),
            Converted::Ignore
        );
    }

    #[test]
    fn stop_reasons_map() {
        assert_eq!(convert_stop_reason("end_turn"), "stop");
        assert_eq!(convert_stop_reason("max_tokens"), "length");
        assert_eq!(convert_stop_reason("tool_use"), "tool_calls");
        assert_eq!(convert_stop_reason("stop_sequence"), "stop");
        assert_eq!(convert_stop_reason("weird"), "stop");
    }

    #[test]
    fn second_message_stop_is_ignored() {
        let mut state = started();
        assert!(matches!(
            convert_event(&mut state, "message_stop", &json!({}), NOW),
            Converted::Emit(_)
        ));
        assert_eq!(
            convert_event(&mut state, "message_stop", &json!({}), NOW),
            Converted::Ignore
        );
    }
}
