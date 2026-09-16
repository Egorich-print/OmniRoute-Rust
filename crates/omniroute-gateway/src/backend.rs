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
