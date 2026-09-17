//! Request/response shapes for the OpenAI-compatible surface.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A single chat message.
///
/// `content` stays raw JSON: OpenAI allows a string, an array of parts, or
/// `null` (an assistant turn that carries only tool calls). `tool_calls` and
/// `tool_call_id` are forwarded verbatim so agent turns round-trip instead of
/// failing deserialization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    /// Thinking text for models that expose it (DeepSeek/OpenRouter style).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ChatMessage {
    /// A message carrying a plain string content.
    pub fn plain(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: Some(Value::from(content.into())),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// A plain assistant text message.
    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: Some(Value::from(text.into())),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// Plain-text view of `content` (string, or concatenated `text` parts).
    pub fn text(&self) -> String {
        match self.content.as_ref() {
            Some(Value::String(text)) => text.clone(),
            Some(Value::Array(parts)) => parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(""),
            _ => String::new(),
        }
    }

    /// Whether the message carries tool calls.
    pub fn has_tool_calls(&self) -> bool {
        self.tool_calls
            .as_ref()
            .and_then(Value::as_array)
            .is_some_and(|calls| !calls.is_empty())
    }
}

/// Incoming `POST /v1/chat/completions` body.
///
/// Unknown fields are ignored so clients can send the full OpenAI payload
/// while the router only consumes what it currently understands.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub tools: Option<Value>,
    #[serde(default)]
    pub tool_choice: Option<Value>,
}

/// One completion choice in a non-streaming response.
#[derive(Debug, Clone, Serialize)]
pub struct ChatChoice {
    pub index: u32,
    pub message: ChatMessage,
    pub finish_reason: String,
}

/// Token accounting, kept OpenAI-shaped.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// Non-streaming `POST /v1/chat/completions` response.
#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: &'static str,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    pub usage: Usage,
}

/// A single streamed delta, format-agnostic.
///
/// The gateway turns these into SSE frames for the client's wire format.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamChunk {
    /// Visible assistant text for this delta, if any.
    pub content: Option<String>,
    /// Reasoning/thinking text for this delta, if any.
    pub reasoning: Option<String>,
    /// OpenAI-shaped `tool_calls` delta array, forwarded verbatim.
    pub tool_calls: Option<serde_json::Value>,
    /// Set on the terminal chunk (`"stop"`, `"length"`, ...).
    pub finish_reason: Option<String>,
}

impl StreamChunk {
    /// A visible-text delta.
    pub fn content(text: impl Into<String>) -> Self {
        Self {
            content: Some(text.into()),
            ..Self::default()
        }
    }

    /// The terminal chunk.
    pub fn done(reason: impl Into<String>) -> Self {
        Self {
            finish_reason: Some(reason.into()),
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ignores_unknown_fields() {
        let raw = r#"{
            "model": "grok-4.6",
            "messages": [{"role": "user", "content": "hi"}],
            "temperature": 0.2,
            "some_future_field": true
        }"#;
        let req: ChatCompletionRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(req.model, "grok-4.6");
        assert_eq!(req.messages.len(), 1);
        assert!(!req.stream);
    }

    #[test]
    fn assistant_tool_call_turn_deserializes() {
        // The exact shape agents send after executing a tool: null content
        // plus tool_calls, followed by a `tool` result message.
        let raw = r#"{
            "model": "grok-4.6",
            "messages": [
                {"role": "user", "content": "time?"},
                {"role": "assistant", "content": null,
                 "tool_calls": [{"id": "call_1", "type": "function",
                                 "function": {"name": "get_time", "arguments": "{\"city\":\"SF\"}"}}]},
                {"role": "tool", "tool_call_id": "call_1", "content": "12:00"}
            ],
            "tools": [{"type": "function",
                       "function": {"name": "get_time", "parameters": {"type": "object"}}}]
        }"#;
        let req: ChatCompletionRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(req.messages.len(), 3);
        assert!(req.messages[1].has_tool_calls());
        assert_eq!(req.messages[1].text(), "");
        assert_eq!(req.messages[2].tool_call_id.as_deref(), Some("call_1"));
        assert!(req.tools.is_some());
    }

    #[test]
    fn message_text_reads_string_and_parts() {
        let plain = ChatMessage {
            role: "user".to_string(),
            content: Some(Value::from("hello")),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        };
        assert_eq!(plain.text(), "hello");

        let parts = ChatMessage {
            role: "user".to_string(),
            content: Some(
                serde_json::json!([{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]),
            ),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        };
        assert_eq!(parts.text(), "ab");
    }

    #[test]
    fn content_chunk_carries_only_text() {
        let chunk = StreamChunk::content("hello");
        assert_eq!(chunk.content.as_deref(), Some("hello"));
        assert!(chunk.reasoning.is_none());
        assert!(chunk.finish_reason.is_none());
    }
}
