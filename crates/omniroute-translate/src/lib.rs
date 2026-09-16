//! Format translators (hub-and-spoke, OpenAI as the pivot).
//!
//! Ports `open-sse/translator/`. Phase 2 starts with the streaming
//! `openai-responses → openai` direction (the grok-cli path): text deltas,
//! reasoning deltas and the terminal `response.completed` event. Tool calls,
//! namespaces and the remaining pairs follow.

pub mod claude;
pub mod claude_request;
pub mod gemini;
pub mod requests;
pub mod responses;
