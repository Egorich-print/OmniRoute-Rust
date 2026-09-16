//! Streaming `gemini → openai` conversion (text path).
//!
//! Mirrors `geminiToOpenAIResponse` in
//! `open-sse/translator/response/gemini-to-openai.ts` for plain text:
//! the first candidate announces the role, text parts become content deltas
//! (`thought:true` parts become `reasoning_content`), and a `finishReason`
//! closes the stream with usage. The Antigravity `{response:{...}}` wrapper
//! is unwrapped; upstream error objects are surfaced, not masked.
//!
//! Explicitly deferred: functionCall/tool synthesis, thought signatures,
//! textual `<think>`/`[Tool call:]` parsers, ANSI stripping, promptFeedback
//! blocks, and the malformed/abort finish-reason overrides.

use serde_json::{Value, json};

/// Mutable per-stream conversion state.
#[derive(Debug, Clone, Default)]
pub struct GeminiState {
    /// Claude-style `chatcmpl-{responseId}` identity, set on first candidate.
    pub message_id: Option<String>,
    /// Model name from `modelVersion`.
    pub model: Option<String>,
    /// Whether the terminal finish chunk already went out.
    pub finish_sent: bool,
    /// Accumulated token accounting.
    pub usage: Option<GeminiUsage>,
    /// Human-readable upstream failure, when the stream carried an error
    /// object instead of candidates.
    pub upstream_error: Option<String>,
}

/// Token accounting in OpenAI shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GeminiUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub cached_tokens: u32,
    pub reasoning_tokens: Option<u32>,
}

/// Convert one Gemini streaming payload to zero or more OpenAI chunks.
///
/// A single payload routinely yields two chunks (role + first delta, or final
/// delta + finish), hence the list. `now` is the Unix timestamp stamped as
/// `created` (injected for deterministic tests).
pub fn convert_event(state: &mut GeminiState, chunk: &Value, now: i64) -> Vec<Value> {
    let response = chunk.get("response").unwrap_or(chunk);
    let candidate = response
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first());

    let Some(candidate) = candidate else {
        record_upstream_error(state, response, chunk);
        return Vec::new();
    };

    let mut out = Vec::new();

    if state.message_id.is_none() {
        state.message_id = response
            .get("responseId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| Some(format!("msg_{now}")));
        state.model = response
            .get("modelVersion")
            .and_then(Value::as_str)
            .map(str::to_string);
        out.push(role_chunk(state, now));
    }

    if let Some(parts) = candidate
        .get("content")
        .and_then(|content| content.get("parts"))
        .and_then(Value::as_array)
    {
        for part in parts {
            let text = part.get("text").and_then(Value::as_str).unwrap_or("");
            if text.is_empty() {
                continue;
            }
            let is_thought = part
                .get("thought")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            out.push(delta_chunk(
                state,
                now,
                if is_thought { None } else { Some(text) },
                if is_thought { Some(text) } else { None },
            ));
        }
    }

    capture_usage(state, response, chunk);

    if let Some(reason) = candidate.get("finishReason").and_then(Value::as_str)
        && !state.finish_sent
    {
        state.finish_sent = true;
        out.push(finish_chunk(state, now, &normalize_finish_reason(reason)));
    }

    out
}

/// Record a mid-stream error object instead of masking it as a clean stop.
fn record_upstream_error(state: &mut GeminiState, response: &Value, chunk: &Value) {
    let error = response
        .get("error")
        .or_else(|| chunk.get("error"))
        .filter(|error| error.is_object());
    let Some(error) = error else { return };
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("Gemini upstream failure")
        .to_string();
    state.upstream_error = Some(message);
}

/// Merge a `usageMetadata` block into the accumulated accounting, mirroring
/// the JS fallbacks (candidates-from-total, thoughts folded into completion).
fn capture_usage(state: &mut GeminiState, response: &Value, chunk: &Value) {
    let meta = response
        .get("usageMetadata")
        .or_else(|| chunk.get("usageMetadata"))
        .filter(|meta| meta.is_object());
    let Some(meta) = meta else { return };

    let number = |key: &str| meta.get(key).and_then(Value::as_u64).unwrap_or(0) as u32;
    let cached = number("cachedContentTokenCount");
    let prompt = number("promptTokenCount");
    let thoughts = number("thoughtsTokenCount");
    let mut candidates = number("candidatesTokenCount");
    let total = number("totalTokenCount");
    if candidates == 0 && total > 0 {
        candidates = total.saturating_sub(prompt).saturating_sub(thoughts);
    }
    let completion = candidates + thoughts;
    state.usage = Some(GeminiUsage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: if total > 0 {
            total
        } else {
            prompt + completion
        },
        cached_tokens: cached,
        reasoning_tokens: (thoughts > 0).then_some(thoughts),
    });
}

/// Lowercase with the OpenAI/safety mappings (`STOP→stop`,
/// `MAX_TOKENS→length`, safety set→`content_filter`).
fn normalize_finish_reason(reason: &str) -> String {
    const OPENAI: &[&str] = &[
        "stop",
        "length",
        "tool_calls",
        "content_filter",
        "function_call",
    ];
    const SAFETY: &[&str] = &[
        "safety",
        "recitation",
        "blocklist",
        "prohibited_content",
        "content_filtered",
        "policy_violation",
        "malformed_response",
    ];
    let normalized = reason.to_lowercase();
    if OPENAI.contains(&normalized.as_str()) {
        return normalized;
    }
    if normalized == "max_tokens" {
        return "length".to_string();
    }
    if SAFETY.contains(&normalized.as_str()) {
        return "content_filter".to_string();
    }
    if normalized.is_empty() {
        return "stop".to_string();
    }
    normalized
}

fn chunk_id(state: &GeminiState) -> String {
    format!(
        "chatcmpl-{}",
        state.message_id.as_deref().unwrap_or("unknown")
    )
}

fn chunk_model(state: &GeminiState) -> String {
    state.model.clone().unwrap_or_else(|| "unknown".to_string())
}

fn role_chunk(state: &GeminiState, now: i64) -> Value {
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
    state: &GeminiState,
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

fn finish_chunk(state: &GeminiState, now: i64, reason: &str) -> Value {
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
        root.insert("prompt_tokens".to_string(), json!(usage.prompt_tokens));
        root.insert(
            "completion_tokens".to_string(),
            json!(usage.completion_tokens),
        );
        root.insert("total_tokens".to_string(), json!(usage.total_tokens));
        if usage.cached_tokens > 0 {
            root.insert(
                "prompt_tokens_details".to_string(),
                json!({ "cached_tokens": usage.cached_tokens }),
            );
        }
        if let Some(reasoning) = usage.reasoning_tokens {
            root.insert(
                "completion_tokens_details".to_string(),
                json!({ "reasoning_tokens": reasoning }),
            );
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

    #[test]
    fn finish_reasons_normalize() {
        assert_eq!(normalize_finish_reason("STOP"), "stop");
        assert_eq!(normalize_finish_reason("MAX_TOKENS"), "length");
        assert_eq!(normalize_finish_reason("SAFETY"), "content_filter");
        assert_eq!(normalize_finish_reason("tool_calls"), "tool_calls");
        assert_eq!(normalize_finish_reason("OTHER"), "other");
        assert_eq!(normalize_finish_reason(""), "stop");
    }

    #[test]
    fn error_object_is_surfaced_not_masked() {
        let mut state = GeminiState::default();
        let out = convert_event(
            &mut state,
            &json!({"error": {"code": 503, "message": "overloaded"}}),
            1700000000,
        );
        assert!(out.is_empty());
        assert_eq!(state.upstream_error.as_deref(), Some("overloaded"));
    }

    #[test]
    fn thought_part_becomes_reasoning() {
        let mut state = GeminiState {
            message_id: Some("m".to_string()),
            model: Some("g".to_string()),
            ..Default::default()
        };
        let out = convert_event(
            &mut state,
            &json!({"candidates": [{"content": {"parts": [{"text": "hmm", "thought": true}]}}]}),
            1700000000,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0]["choices"][0]["delta"]["reasoning_content"],
            json!("hmm")
        );
    }
}
