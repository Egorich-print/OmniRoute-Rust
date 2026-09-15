//! Gateway error type.

use std::fmt;

/// Errors surfaced by a backend while serving a completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayError {
    /// The requested model is not known to any configured provider.
    UnknownModel(String),
    /// The upstream provider rejected or failed the request.
    Upstream(String),
    /// The request was malformed.
    InvalidRequest(String),
}

impl fmt::Display for GatewayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownModel(model) => write!(f, "unknown model: {model}"),
            Self::Upstream(msg) => write!(f, "upstream error: {msg}"),
            Self::InvalidRequest(msg) => write!(f, "invalid request: {msg}"),
        }
    }
}

impl std::error::Error for GatewayError {}
