//! Chat backends.
//!
//! A [`ChatBackend`] abstracts "something that can answer a chat completion".
//! The first implementation is [`EchoBackend`], a deterministic in-process
//! backend used to prove out the HTTP/SSE surface before real provider
//! executors (grok-cli, OpenAI, Anthropic, ...) are ported over.

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use omniroute_core::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, GatewayError,
    StreamChunk, Usage,
};
use serde_json::{Value, json};

/// A boxed, owned stream of deltas.
pub type ChunkStream = BoxStream<'static, Result<StreamChunk, GatewayError>>;

/// Anything that can serve a chat completion.
#[async_trait]
pub trait ChatBackend: Send + Sync + 'static {
    /// Human-readable backend name, used in logs and health output.
    fn name(&self) -> &'static str;

    /// Serve a complete (non-streaming) response.
    async fn complete(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError>;

    /// Serve a streaming response as a sequence of deltas.
    fn stream(&self, request: ChatCompletionRequest) -> ChunkStream;
}

/// Build an OpenAI-shaped request body from a typed request.
///
/// Shared by executors that speak OpenAI Chat Completions natively or via a
/// translator that consumes the same shape.
pub(crate) fn openai_request_body(request: &ChatCompletionRequest) -> Value {
    let messages: Vec<Value> = request
        .messages
        .iter()
        .map(|message| json!({"role": message.role, "content": message.content}))
        .collect();
    let mut body = json!({
        "model": request.model,
        "messages": messages,
        "stream": request.stream,
    });
    if let Some(max_tokens) = request.max_tokens {
        body["max_tokens"] = json!(max_tokens);
    }
    if let Some(temperature) = request.temperature {
        body["temperature"] = json!(temperature);
    }
    body
}

/// Reduce an OpenAI `chat.completion.chunk` object to a gateway `StreamChunk`.
///
/// Returns `None` for chunks without content, reasoning or finish signal.
pub(crate) fn openai_chunk_to_stream_chunk(value: &Value) -> Option<StreamChunk> {
    let choice = value.get("choices")?.get(0)?;
    let delta = choice.get("delta")?;
    // Empty-string content is a placeholder some upstreams (OpenRouter free
    // tiers) stream alongside reasoning; it must not become a delta.
    let content = delta
        .get("content")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    // OpenRouter uses `reasoning`; OpenAI-compatible proxies use
    // `reasoning_content`. Accept both.
    let reasoning = delta
        .get("reasoning_content")
        .or_else(|| delta.get("reasoning"))
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let finish = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .map(str::to_string);
    let tool_calls = delta.get("tool_calls").cloned();
    if content.is_none() && reasoning.is_none() && tool_calls.is_none() && finish.is_none() {
        return None;
    }
    Some(StreamChunk {
        content,
        reasoning,
        tool_calls,
        finish_reason: finish,
    })
}

/// Deterministic backend that echoes the last user message.
///
/// Useful as a smoke target and as a reference implementation for the
/// streaming contract; it never touches the network.
#[derive(Debug, Default, Clone)]
pub struct EchoBackend;

impl EchoBackend {
    fn reply(request: &ChatCompletionRequest) -> String {
        let last_user = request
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.content.clone())
            .unwrap_or_default();
        format!("echo: {last_user}")
    }
}

#[async_trait]
impl ChatBackend for EchoBackend {
    fn name(&self) -> &'static str {
        "echo"
    }

    async fn complete(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        if request.messages.is_empty() {
            return Err(GatewayError::InvalidRequest("messages is empty".into()));
        }

        let reply = Self::reply(&request);
        let prompt_tokens = request
            .messages
            .iter()
            .map(|m| m.content.len())
            .sum::<usize>() as u32;
        let completion_tokens = reply.len() as u32;

        Ok(ChatCompletionResponse {
            id: crate::ids::completion_id(),
            object: "chat.completion",
            created: crate::ids::unix_seconds(),
            model: request.model,
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".into(),
                    content: reply,
                },
                finish_reason: "stop".into(),
            }],
            usage: Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
            },
        })
    }

    fn stream(&self, request: ChatCompletionRequest) -> ChunkStream {
        if request.messages.is_empty() {
            return Box::pin(futures::stream::once(async {
                Err(GatewayError::InvalidRequest("messages is empty".into()))
            }));
        }

        let words: Vec<String> = Self::reply(&request)
            .split_inclusive(' ')
            .map(|word| word.to_string())
            .collect();

        let deltas = futures::stream::iter(
            words
                .into_iter()
                .map(|word| Ok(StreamChunk::content(word)))
                .collect::<Vec<Result<StreamChunk, GatewayError>>>(),
        );

        Box::pin(deltas.chain(futures::stream::once(async {
            Ok(StreamChunk::done("stop"))
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_mapping_carries_tool_calls() {
        let chunk = openai_chunk_to_stream_chunk(&json!({
            "choices": [{
                "index": 0,
                "delta": {"tool_calls": [{"index": 0, "function": {"arguments": "{}"}}]},
                "finish_reason": null,
            }],
        }))
        .expect("chunk present");
        assert!(chunk.tool_calls.is_some());
        assert!(chunk.content.is_none());
    }

    #[test]
    fn chunk_mapping_accepts_openrouter_reasoning() {
        let chunk = openai_chunk_to_stream_chunk(&json!({
            "choices": [{
                "index": 0,
                "delta": {"content": "", "role": "assistant", "reasoning": "thinking"},
                "finish_reason": null,
            }],
        }))
        .expect("reasoning chunk present");
        assert_eq!(chunk.reasoning.as_deref(), Some("thinking"));
        assert!(chunk.content.is_none(), "empty content must be dropped");
    }

    #[test]
    fn chunk_mapping_drops_empty_deltas() {
        assert!(
            openai_chunk_to_stream_chunk(&json!({
                "choices": [{"index": 0, "delta": {}, "finish_reason": null}],
            }))
            .is_none()
        );
    }

    fn request(text: &str) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "test-model".into(),
            messages: vec![ChatMessage {
                role: "user".into(),
                content: text.into(),
            }],
            stream: false,
            max_tokens: None,
            temperature: None,
        }
    }

    #[tokio::test]
    async fn complete_echoes_last_user_message() {
        let backend = EchoBackend;
        let response = backend.complete(request("hello")).await.unwrap();
        assert_eq!(response.choices[0].message.content, "echo: hello");
        assert_eq!(response.choices[0].finish_reason, "stop");
    }

    #[tokio::test]
    async fn complete_rejects_empty_messages() {
        let backend = EchoBackend;
        let mut req = request("x");
        req.messages.clear();
        assert_eq!(
            backend.complete(req).await.unwrap_err(),
            GatewayError::InvalidRequest("messages is empty".into())
        );
    }

    #[tokio::test]
    async fn stream_ends_with_finish_reason() {
        use futures::StreamExt;

        let backend = EchoBackend;
        let chunks: Vec<_> = backend
            .stream(request("hi"))
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();

        assert_eq!(
            chunks.last().unwrap().finish_reason.as_deref(),
            Some("stop")
        );
        let text: String = chunks
            .iter()
            .filter_map(|c| c.content.clone())
            .collect::<Vec<_>>()
            .join("");
        assert_eq!(text, "echo: hi");
    }
}
