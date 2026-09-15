//! Request/response shapes for the OpenAI-compatible surface.

use serde::{Deserialize, Serialize};

/// A single chat message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamChunk {
    /// Visible assistant text for this delta, if any.
    pub content: Option<String>,
    /// Reasoning/thinking text for this delta, if any.
    pub reasoning: Option<String>,
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
    fn content_chunk_carries_only_text() {
        let chunk = StreamChunk::content("hello");
        assert_eq!(chunk.content.as_deref(), Some("hello"));
        assert!(chunk.reasoning.is_none());
        assert!(chunk.finish_reason.is_none());
    }
}
