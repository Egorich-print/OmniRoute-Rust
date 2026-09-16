//! Shared async HTTP client.
//!
//! The explicit replacement for the JS ambient `fetch` patch
//! (`open-sse/utils/proxyFetch.ts`). Every executor builds requests through
//! this client with an explicit base URL, bearer token and timeout — proxy,
//! TLS profile and capture context travel as struct fields, never as globals.

pub mod client;
pub mod sse;

pub use client::HttpClient;
pub use sse::{SseDecoder, SseEvent};
