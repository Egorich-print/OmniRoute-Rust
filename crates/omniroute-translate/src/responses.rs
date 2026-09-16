//! Streaming `openai-responses → openai` conversion.
//!
//! Mirrors `openaiResponsesToOpenAIResponseStream` in
//! `open-sse/translator/response/openai-responses.ts`: text and reasoning
//! deltas, incremental function calls (`.added` announces with index/id,
//! `.delta` buffers arguments, `.done` emits them), and the terminal
//! `response.completed` event.
//!
//! Explicitly deferred: `completed`-snapshot tool synthesis, namespaces,
//! Agent argument normalisation, and reasoning-summary deltas beyond the
//! plain `reasoning_content` mapping. Unknown event kinds are ignored,
//! never fabricated.

use serde_json::{Value, json};
use std::collections::HashMap;

/// In-flight function call tracked across `.added`/`.delta`/`.done` events.
#[derive(Debug, Clone, Default)]
pub struct ToolCallEntry {
    /// Assigned OpenAI index (`None` until the name resolves).
    pub index: Option<u32>,
    /// Normalized tool name (empty while deferred).
    pub name: String,
    /// Buffered `arguments` fragments.
    pub args: String,
}

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
    /// Next tool-call index to assign.
    pub tool_call_index: u32,
    /// In-flight calls by `call_id`.
    pub tool_calls: HashMap<String, ToolCallEntry>,
    /// `item.id` → `call_id` reverse map.
    pub item_to_call: HashMap<String, String>,
    /// `output_index` → `call_id` reverse map.
    pub output_index_to_call: HashMap<i64, String>,
    /// Most recently opened call, fallback for unidentified deltas.
    pub current_tool_call: Option<String>,
    /// Monotonic fallback id sequence (deterministic; JS uses timestamps).
    pub fallback_seq: u64,
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
            tool_call_index: 0,
            tool_calls: HashMap::new(),
            item_to_call: HashMap::new(),
            output_index_to_call: HashMap::new(),
            current_tool_call: None,
            fallback_seq: 0,
        }
    }

    fn fallback_call_id(&mut self) -> String {
        self.fallback_seq += 1;
        format!("call_fallback_{}", self.fallback_seq)
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
        "response.output_item.added" => {
            let Some(item) = data.get("item") else {
                return Converted::Ignore;
            };
            if item.get("type").and_then(Value::as_str) != Some("function_call") {
                return Converted::Ignore;
            }
            let mut call_id = item
                .get("call_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if call_id.is_empty() {
                call_id = state.fallback_call_id();
            }
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if let Some(item_id) = item.get("id").and_then(Value::as_str) {
                state
                    .item_to_call
                    .insert(item_id.to_string(), call_id.clone());
            }
            if let Some(output_index) = data.get("output_index").and_then(Value::as_i64) {
                state
                    .output_index_to_call
                    .insert(output_index, call_id.clone());
            }
            state.current_tool_call = Some(call_id.clone());

            let entry = state
                .tool_calls
                .entry(call_id.clone())
                .or_insert_with(|| ToolCallEntry {
                    index: None,
                    name: name.clone(),
                    args: String::new(),
                });
            entry.name = name.clone();
            if name.is_empty() {
                // Deferred: emit once output_item.done resolves a name.
                return Converted::Ignore;
            }
            if entry.index.is_none() {
                entry.index = Some(state.tool_call_index);
                state.tool_call_index += 1;
            }
            let index = entry.index.unwrap_or(0);
            Converted::Emit(tool_chunk(
                state,
                json!([{
                    "index": index,
                    "id": call_id,
                    "type": "function",
                    "function": {"name": name, "arguments": ""},
                }]),
            ))
        }
        "response.function_call_arguments.delta" => {
            let text = data.get("delta").and_then(Value::as_str).unwrap_or("");
            if text.is_empty() {
                return Converted::Ignore;
            }
            // Resolve the in-flight call: item_id, then output_index, then the
            // single open call, then the most recent one. Buffered, never
            // emitted directly (see output_item.done).
            let mut call_id: Option<String> = None;
            if let Some(item_id) = data.get("item_id").and_then(Value::as_str) {
                call_id = state.item_to_call.get(item_id).cloned();
            }
            if call_id.is_none()
                && let Some(output_index) = data.get("output_index").and_then(Value::as_i64)
            {
                call_id = state.output_index_to_call.get(&output_index).cloned();
            }
            if call_id.is_none() {
                if state.tool_calls.len() == 1 {
                    call_id = state.tool_calls.keys().next().cloned();
                } else {
                    call_id = state.current_tool_call.clone();
                }
            }
            if let Some(id) = call_id
                && let Some(entry) = state.tool_calls.get_mut(&id)
            {
                entry.args.push_str(text);
            }
            Converted::Ignore
        }
        "response.output_item.done" => {
            let Some(item) = data.get("item") else {
                return Converted::Ignore;
            };
            if item.get("type").and_then(Value::as_str) != Some("function_call") {
                return Converted::Ignore;
            }
            let mut call_id = item
                .get("call_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            if call_id.is_none()
                && let Some(item_id) = item.get("id").and_then(Value::as_str)
            {
                call_id = state.item_to_call.get(item_id).cloned();
            }
            if call_id.is_none() {
                call_id = state.current_tool_call.clone();
            }
            let call_id = call_id.unwrap_or_else(|| state.fallback_call_id());
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();

            // Claim an index if .added never assigned one.
            let index = match state.tool_calls.get_mut(&call_id) {
                Some(entry) => {
                    if entry.index.is_none() && !name.is_empty() {
                        entry.index = Some(state.tool_call_index);
                        state.tool_call_index += 1;
                    }
                    if !name.is_empty() {
                        entry.name = name.clone();
                    }
                    entry.index
                }
                None => {
                    if name.is_empty() {
                        return Converted::Ignore;
                    }
                    let index = state.tool_call_index;
                    state.tool_call_index += 1;
                    state.tool_calls.insert(
                        call_id.clone(),
                        ToolCallEntry {
                            index: Some(index),
                            name: name.clone(),
                            args: String::new(),
                        },
                    );
                    Some(index)
                }
            };
            let Some(index) = index else {
                // Deferred call whose name never resolved.
                state.tool_calls.remove(&call_id);
                if state.current_tool_call.as_deref() == Some(call_id.as_str()) {
                    state.current_tool_call = None;
                }
                return Converted::Ignore;
            };

            // Prefer the completed arguments; fall back to buffered deltas.
            // (Schema-aware null normalisation is deferred; it only affects
            // the Agent tool family.)
            let buffered = state
                .tool_calls
                .get(&call_id)
                .map(|entry| entry.args.clone())
                .unwrap_or_default();
            let item_args = item.get("arguments").and_then(Value::as_str).unwrap_or("");
            let arguments = if !item_args.is_empty() {
                item_args.to_string()
            } else {
                buffered
            };

            state.tool_calls.remove(&call_id);
            if state.current_tool_call.as_deref() == Some(call_id.as_str()) {
                state.current_tool_call = None;
            }

            Converted::Emit(tool_chunk(
                state,
                json!([{
                    "index": index,
                    "function": {"arguments": arguments},
                }]),
            ))
        }
        "response.completed" => {
            if state.finish_sent {
                return Converted::Ignore;
            }
            state.finish_sent = true;
            let usage = extract_usage(data);
            Converted::Emit(finish_chunk(state, compute_finish_reason(state), usage))
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

/// Terminal reason: `tool_calls` once any call opened, else `stop`.
fn compute_finish_reason(state: &ResponsesState) -> &'static str {
    if state.tool_call_index > 0 || state.current_tool_call.is_some() {
        "tool_calls"
    } else {
        "stop"
    }
}

/// Build a tool-calls delta chunk, announcing the role on the first chunk.
fn tool_chunk(state: &mut ResponsesState, tool_calls: Value) -> Value {
    let mut delta = serde_json::Map::new();
    if !state.role_sent {
        delta.insert("role".to_string(), Value::from("assistant"));
        state.role_sent = true;
    }
    delta.insert("tool_calls".to_string(), tool_calls);
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
