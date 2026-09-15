//! Wire formats handled by the gateway.
//!
//! Mirrors the `FORMATS` vocabulary the TypeScript implementation used
//! (`openai`, `openai-responses`, `claude`, `gemini`, `antigravity`) so the
//! translation matrix can be ported one pair at a time.

use serde::{Deserialize, Serialize};

/// A client or provider wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WireFormat {
    /// OpenAI Chat Completions (`/v1/chat/completions`).
    OpenAi,
    /// OpenAI Responses API (`/v1/responses`).
    OpenAiResponses,
    /// Anthropic Messages API.
    Claude,
    /// Google Gemini `generateContent` / `streamGenerateContent`.
    Gemini,
    /// Antigravity / Google Cloud Code wrapper format.
    Antigravity,
}

impl WireFormat {
    /// Stable string used in logs and JSON payloads.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::OpenAiResponses => "openai-responses",
            Self::Claude => "claude",
            Self::Gemini => "gemini",
            Self::Antigravity => "antigravity",
        }
    }
}

/// Whether a response coming from `upstream` must be translated before it is
/// handed to a client that speaks `client`.
///
/// Translation is only skipped when both sides already agree on the shape.
pub const fn needs_translation(upstream: WireFormat, client: WireFormat) -> bool {
    !matches!((upstream, client), (WireFormat::OpenAi, WireFormat::OpenAi))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_openai_format_needs_no_translation() {
        assert!(!needs_translation(WireFormat::OpenAi, WireFormat::OpenAi));
    }

    #[test]
    fn responses_to_openai_is_translated() {
        assert!(needs_translation(
            WireFormat::OpenAiResponses,
            WireFormat::OpenAi
        ));
    }

    #[test]
    fn claude_to_openai_is_translated() {
        assert!(needs_translation(WireFormat::Claude, WireFormat::OpenAi));
    }

    #[test]
    fn format_strings_are_stable() {
        assert_eq!(WireFormat::OpenAiResponses.as_str(), "openai-responses");
        assert_eq!(WireFormat::Antigravity.as_str(), "antigravity");
    }
}
