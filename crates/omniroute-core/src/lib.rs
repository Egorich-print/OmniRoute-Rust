//! Core domain types for the OmniRoute Rust rewrite.
//!
//! This crate is intentionally dependency-light: it holds the wire-format
//! vocabulary and the request/response shapes shared by the gateway, the
//! desktop shell and future provider crates. No HTTP, no async runtime.

pub mod chat;
pub mod error;
pub mod format;

pub use chat::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, StreamChunk, Usage,
};
pub use error::GatewayError;
pub use format::{WireFormat, needs_translation};
