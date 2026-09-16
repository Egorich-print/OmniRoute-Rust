//! OpenAI chat → Claude Messages request conversion (text path).
//!
//! Mirrors `openaiToClaudeRequest` in
//! `open-sse/translator/request/openai-to-claude.ts` for plain text:
//! system/developer hoisting with cache control, role merging, text blocks,
//! sampling params, stop sequences and the empty-messages guard.
//!
//! Explicitly deferred: thinking/reasoning configuration, tools and
//! tool_choice, images/files, `body.system` merging, cache_control beyond
//! the two prescribed spots, and Kimi-specific paths. Tool-carrying requests
//! are out of scope for this slice (see the `tools` note below).

use serde_json::{Map, Value, json};

const DEFAULT_MAX_TOKENS: u32 = 64000;
const DEFAULT_MIN_TOKENS: u32 = 32000;

/// Whether the model forces extended thinking server-side, in which case
/// `temperature` must be stripped (mirrors the JS `/claude-(?:opus|sonnet)-4/i`
/// test).
fn model_forces_thinking(model: &str) -> bool {
    let lower = model.to_lowercase();
    lower.contains("claude-opus-4") || lower.contains("claude-sonnet-4")
}

/// `adjustMaxTokens`: `max_tokens ?? max_completion_tokens`, defaulted,
/// raised for tool calls, clamped to at least 1.
fn adjust_max_tokens(body: &Value, has_tools: bool) -> u32 {
    let requested = body
        .get("max_tokens")
        .or_else(|| body.get("max_completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(u64::from(DEFAULT_MAX_TOKENS)) as u32;
    let mut max_tokens = if requested == 0 {
        DEFAULT_MAX_TOKENS
    } else {
        requested
    };
    if has_tools && max_tokens < DEFAULT_MIN_TOKENS {
        max_tokens = DEFAULT_MIN_TOKENS;
    }
    max_tokens.max(1)
}

/// `normalizeContentToString`: string as-is, text parts joined with `\n`.
fn normalize_content_to_string(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .map(|part| match part.get("text") {
                Some(Value::String(text)) => text.clone(),
                Some(Value::Number(number)) => number.to_string(),
                Some(Value::Bool(flag)) => flag.to_string(),
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Message content → Claude text blocks. Empty strings and empty text parts
/// are dropped, matching the JS truthiness checks.
fn text_blocks(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::String(text)) if !text.is_empty() => {
            vec![json!({"type": "text", "text": text})]
        }
        Some(Value::Array(parts)) => parts
            .iter()
            .filter(|part| {
                part.get("type").and_then(Value::as_str) == Some("text")
                    && part
                        .get("text")
                        .and_then(Value::as_str)
                        .is_some_and(|text| !text.is_empty())
            })
            .map(|part| {
                json!({
                    "type": "text",
                    "text": part.get("text").and_then(Value::as_str).unwrap_or(""),
                })
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Flush the accumulated turn into `messages` when non-empty.
fn flush_turn(messages: &mut Vec<Value>, role: &mut Option<String>, blocks: &mut Vec<Value>) {
    if let Some(role) = role.take()
        && !blocks.is_empty()
    {
        messages.push(json!({
            "role": role,
            "content": std::mem::take(blocks),
        }));
    }
}

/// Convert an OpenAI chat request body to a Claude Messages request body.
///
/// `model` is stamped through and `stream` passed through. Tool-carrying,
/// thinking and multimodal inputs are deferred (see module docs).
pub fn openai_to_claude(model: &str, body: &Value, stream: bool) -> Value {
    let has_tools = body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty());

    let mut result = Map::new();
    result.insert("model".to_string(), Value::from(model));
    result.insert(
        "max_tokens".to_string(),
        json!(adjust_max_tokens(body, has_tools)),
    );
    result.insert("stream".to_string(), Value::from(stream));

    if let Some(temperature) = body.get("temperature")
        && !model_forces_thinking(model)
    {
        result.insert("temperature".to_string(), temperature.clone());
    } else if body.get("temperature").is_none()
        && let Some(top_p) = body.get("top_p")
    {
        result.insert("top_p".to_string(), top_p.clone());
    }
    if let Some(stop) = body.get("stop") {
        let sequences = match stop {
            Value::Array(items) => items.clone(),
            _ => vec![stop.clone()],
        };
        result.insert("stop_sequences".to_string(), Value::Array(sequences));
    }

    let mut system_parts: Vec<String> = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    let mut current_role: Option<String> = None;
    let mut current_blocks: Vec<Value> = Vec::new();

    if let Some(list) = body.get("messages").and_then(Value::as_array) {
        for message in list {
            let role = message.get("role").and_then(Value::as_str).unwrap_or("");
            if role == "system" || role == "developer" {
                system_parts.push(normalize_content_to_string(message.get("content")));
                continue;
            }

            let new_role = if role == "user" || role == "tool" {
                "user"
            } else {
                "assistant"
            }
            .to_string();
            // Tool-result separation and image/file parts are deferred with
            // tools; tool-role turns currently ride along as user text.
            let blocks = text_blocks(message.get("content"));

            if current_role.as_deref() != Some(new_role.as_str()) {
                flush_turn(&mut messages, &mut current_role, &mut current_blocks);
                current_role = Some(new_role);
            }
            current_blocks.extend(blocks);
        }
    }
    flush_turn(&mut messages, &mut current_role, &mut current_blocks);

    // Drop assistant messages left with empty content.
    messages.retain(|message| {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            return true;
        }
        message
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|content| !content.is_empty())
    });

    // Cache the last block of the last assistant message.
    for message in messages.iter_mut().rev() {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        if let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut)
            && let Some(last) = blocks.last_mut()
        {
            if let Some(object) = last.as_object_mut() {
                object.insert("cache_control".to_string(), json!({"type": "ephemeral"}));
            }
            break;
        }
    }

    if !system_parts.is_empty() {
        let system_block = json!({
            "type": "text",
            "text": system_parts.join("\n"),
            "cache_control": {"type": "ephemeral", "ttl": "1h"},
        });
        // body.system merging is deferred with the multimodal paths; the
        // fixtures only exercise role="system" messages.
        result.insert("system".to_string(), Value::Array(vec![system_block]));
    }

    if messages.is_empty() {
        messages.push(json!({
            "role": "user",
            "content": [{"type": "text", "text": "."}],
        }));
    }
    result.insert("messages".to_string(), Value::Array(messages));

    Value::Object(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_tokens_defaults_and_floors() {
        assert_eq!(adjust_max_tokens(&json!({}), false), 64000);
        assert_eq!(adjust_max_tokens(&json!({"max_tokens": 0}), false), 64000);
        assert_eq!(
            adjust_max_tokens(&json!({"max_completion_tokens": 50}), false),
            50
        );
        assert_eq!(adjust_max_tokens(&json!({"max_tokens": 100}), true), 32000);
    }

    #[test]
    fn thinking_models_match() {
        assert!(model_forces_thinking("claude-sonnet-4-6"));
        assert!(model_forces_thinking("CLAUDE-OPUS-4-1"));
        assert!(!model_forces_thinking("claude-haiku-4-5"));
        assert!(!model_forces_thinking("gpt-4o"));
    }
}
