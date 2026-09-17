//! Bearer API-key authentication.
//!
//! Mirrors the JS `validateApiKey` contract: when the `api_keys` table holds
//! at least one key (or `require_auth` is set), `/v1/*` requires
//! `Authorization: Bearer <key>`. A fresh database with no keys stays open so
//! first-run provisioning never locks the operator out.
//!
//! Implemented as an extractor (not middleware) so each handler opts in
//! explicitly and `/healthz` stays public by construction.

use axum::{
    Json,
    extract::FromRequestParts,
    http::{StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::AppState;

/// The API key a request authenticated with.
#[derive(Debug, Clone)]
pub struct AuthKey {
    /// Row id in `api_keys`.
    pub id: String,
    /// Display name of the key.
    pub name: String,
}

/// Extractor yielding the authenticated key, or `None` when auth is open.
///
/// Rejects with `401` JSON when keys exist but the request carries no (or a
/// wrong) bearer token.
pub struct Authenticated {
    /// Present when the request authenticated against a non-empty key table.
    pub key: Option<AuthKey>,
}

impl FromRequestParts<AppState> for Authenticated {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let Some(db) = state.db.as_ref() else {
            return Ok(Self { key: None });
        };

        let guard = db.connection();
        let key_count: i64 = guard
            .query_row("SELECT COUNT(*) FROM api_keys", [], |row| row.get(0))
            .unwrap_or(0);
        if key_count == 0 && !state.require_auth {
            return Ok(Self { key: None });
        }

        let Some(token) = bearer_token(parts) else {
            return Err(unauthorized("missing bearer token"));
        };
        match omniroute_db::repos::find_usable_api_key(&guard, &token).unwrap_or(None) {
            Some(row) => Ok(Self {
                key: Some(AuthKey {
                    id: row.id,
                    name: row.name,
                }),
            }),
            // Unknown, revoked, banned, deactivated or expired.
            None => Err(unauthorized("invalid api key")),
        }
    }
}

fn bearer_token(parts: &Parts) -> Option<String> {
    let value = parts.headers.get(header::AUTHORIZATION)?;
    let text = value.to_str().ok()?;
    let (scheme, token) = text.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_string())
}

fn unauthorized(message: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": {
                "message": message,
                "type": "invalid_request_error",
                "code": "invalid_api_key",
            }
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use omniroute_db::{Db, repos::ApiKeyRow};

    fn keyed_db() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let guard = db.connection();
        omniroute_db::repos::insert_api_key(
            &guard,
            &ApiKeyRow {
                id: "k1".to_string(),
                name: "test".to_string(),
                key: "sk-test-123".to_string(),
                allowed_models: "[\"*\"]".to_string(),
                no_log: false,
            },
        )
        .unwrap();
        drop(guard);
        db
    }

    fn parts_with_auth(value: Option<&str>) -> Parts {
        let mut builder = Request::builder();
        if let Some(value) = value {
            builder = builder.header(header::AUTHORIZATION, value);
        }
        let (parts, _) = builder.body(Body::empty()).unwrap().into_parts();
        parts
    }

    #[test]
    fn bearer_parsing_accepts_case_insensitive_scheme() {
        let parts = parts_with_auth(Some("Bearer sk-test-123"));
        assert_eq!(bearer_token(&parts).as_deref(), Some("sk-test-123"));

        let lower = parts_with_auth(Some("bearer sk-test-123"));
        assert_eq!(bearer_token(&lower).as_deref(), Some("sk-test-123"));

        let basic = parts_with_auth(Some("Basic abc"));
        assert_eq!(bearer_token(&basic), None);

        let missing = parts_with_auth(None);
        assert_eq!(bearer_token(&missing), None);
    }

    #[test]
    fn keyed_db_lookup_finds_inserted_key() {
        let db = keyed_db();
        let guard = db.connection();
        let row = omniroute_db::repos::find_usable_api_key(&guard, "sk-test-123")
            .unwrap()
            .unwrap();
        assert_eq!(row.id, "k1");
    }

    #[test]
    fn revoked_key_stops_authenticating() {
        let db = keyed_db();
        db.migrate().unwrap();
        let guard = db.connection();
        assert!(
            omniroute_db::repos::find_usable_api_key(&guard, "sk-test-123")
                .unwrap()
                .is_some()
        );
        omniroute_db::repos::set_api_key_revoked(&guard, "k1", true).unwrap();
        assert!(
            omniroute_db::repos::find_usable_api_key(&guard, "sk-test-123")
                .unwrap()
                .is_none()
        );
    }
}
