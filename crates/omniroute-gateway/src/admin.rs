//! Read-only admin API backing the dashboard.
//!
//! Serves what the JS dashboard showed, over the same SQLite the gateway
//! already uses: which providers are live, how combos resolve, recent usage
//! and recent calls. Secrets are never serialised — only whether a credential
//! exists. Mutations (editing combos, keys, providers) are out of scope for
//! this first contour.

use axum::{
    Json,
    extract::{Query, State},
};
use omniroute_db::repos;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::auth::Authenticated;
use crate::combos::{dispatch_backend, load_combos};

/// Query for the time-ranged endpoints.
#[derive(Debug, Deserialize)]
pub struct ListQuery {
    /// Window length in days (clamped to 1..=90). Defaults to 7.
    pub days: Option<u32>,
    /// Maximum rows to return (clamped to 1..=500). Defaults to 100.
    pub limit: Option<u32>,
}

impl ListQuery {
    /// SQLite datetime modifier for the requested window.
    fn since(&self) -> String {
        format!("-{} days", self.days.unwrap_or(7).clamp(1, 90))
    }

    fn limit(&self) -> usize {
        self.limit.unwrap_or(100).clamp(1, 500) as usize
    }
}

/// `GET /api/overview` — gateway identity, live backends and row counts.
pub async fn overview(State(state): State<AppState>, _auth: Authenticated) -> Json<Value> {
    let mut combos = 0;
    let mut connections_active = 0;
    let mut connections_total = 0;
    let mut api_keys = 0;

    if let Some(db) = state.db.as_ref() {
        let guard = db.connection();
        combos = repos::list_combos(&guard)
            .map(|rows| rows.len())
            .unwrap_or(0);
        if let Ok(rows) = repos::list_connections(&guard) {
            connections_total = rows.len();
            connections_active = rows.iter().filter(|row| row.is_active).count();
        }
        api_keys = repos::count_active_api_keys(&guard).unwrap_or(0);
    }

    let mut backends = state.backend_names.clone();
    backends.sort();
    backends.dedup();

    Json(json!({
        "status": "ok",
        "backend": state.backend.name(),
        "backends": backends,
        "counts": {
            "combos": combos,
            "connections_active": connections_active,
            "connections_total": connections_total,
            "api_keys": api_keys,
        },
    }))
}

/// `GET /api/combos` — every combo with each entry's dispatch verdict.
///
/// `servable` per entry comes from the router's own predicate, so the UI shows
/// exactly what the gateway would do.
pub async fn combos(State(state): State<AppState>, _auth: Authenticated) -> Json<Value> {
    let Some(db) = state.db.as_ref() else {
        return Json(json!({ "combos": [] }));
    };
    let Some(registry) = state.registry.as_ref() else {
        return Json(json!({ "combos": [] }));
    };

    let available: std::collections::HashSet<String> =
        state.backend_names.iter().cloned().collect();
    let guard = db.connection();
    let parsed = load_combos(&guard);
    let mut out: Vec<Value> = Vec::new();

    if let Ok(rows) = repos::list_combos(&guard) {
        for row in rows {
            let combo = parsed.get(&row.name);
            let entries: Vec<Value> = combo
                .map(|combo| {
                    combo
                        .entries
                        .iter()
                        .map(|entry| {
                            let backend = dispatch_backend(entry, registry, &available);
                            json!({
                                "provider": entry.provider,
                                "model": entry.model,
                                "backend": backend,
                                "servable": backend.is_some(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            out.push(json!({
                "name": row.name,
                "strategy": combo.map(|combo| combo.strategy.clone()).unwrap_or_default(),
                "servable_entries": entries.iter().filter(|entry| entry["servable"] == json!(true)).count(),
                "total_entries": entries.len(),
                "entries": entries,
            }));
        }
    }

    Json(json!({ "combos": out }))
}

/// `GET /api/connections` — providers and their connections, secrets masked.
pub async fn connections(State(state): State<AppState>, _auth: Authenticated) -> Json<Value> {
    let Some(db) = state.db.as_ref() else {
        return Json(json!({ "connections": [] }));
    };

    let servable: std::collections::HashSet<String> = {
        let mut set = std::collections::HashSet::new();
        if let Some(registry) = state.registry.as_ref() {
            for name in &state.backend_names {
                // Backend names are canonical ids; also expose aliases that
                // point at them so the UI can match stored provider strings.
                set.insert(name.clone());
                for entry in registry.iter() {
                    if &entry.id == name
                        && let Some(alias) = entry.alias.as_deref()
                    {
                        set.insert(alias.to_string());
                    }
                }
            }
        }
        set
    };

    let guard = db.connection();
    let rows = match repos::list_connections(&guard) {
        Ok(rows) => rows,
        Err(error) => {
            return Json(json!({ "connection_error": error.to_string(), "connections": [] }));
        }
    };

    let connections: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "id": row.id,
                "provider": row.provider,
                "name": row.name,
                "auth_type": row.auth_type,
                "is_active": row.is_active,
                "priority": row.priority,
                "expires_at": row.expires_at,
                "test_status": row.test_status,
                "last_error": row.last_error,
                "has_credential": row.has_credential,
                "servable": servable.contains(&row.provider),
            })
        })
        .collect();

    Json(json!({ "connections": connections }))
}

/// `GET /api/usage?days=7&limit=10` — totals plus top providers and models.
pub async fn usage(
    State(state): State<AppState>,
    _auth: Authenticated,
    Query(query): Query<ListQuery>,
) -> Json<Value> {
    let Some(db) = state.db.as_ref() else {
        return Json(json!({ "usage": null }));
    };
    let guard = db.connection();
    let since = query.since();
    let limit = query.limit().min(50);

    let summary = match repos::usage_summary(&guard, &since, limit) {
        Ok(summary) => summary,
        Err(error) => return Json(json!({ "error": error.to_string() })),
    };

    let buckets = |rows: &[repos::UsageBucket]| -> Vec<Value> {
        rows.iter()
            .map(|bucket| {
                json!({
                    "key": bucket.key,
                    "requests": bucket.requests,
                    "tokens_input": bucket.tokens_input,
                    "tokens_output": bucket.tokens_output,
                })
            })
            .collect()
    };

    Json(json!({
        "window_days": query.days.unwrap_or(7).clamp(1, 90),
        "requests": summary.requests,
        "successes": summary.successes,
        "tokens_input": summary.tokens_input,
        "tokens_output": summary.tokens_output,
        "tokens_cache_read": summary.tokens_cache_read,
        "by_provider": buckets(&summary.by_provider),
        "by_model": buckets(&summary.by_model),
    }))
}

/// `GET /api/logs?limit=50` — recent calls, newest first.
pub async fn logs(
    State(state): State<AppState>,
    _auth: Authenticated,
    Query(query): Query<ListQuery>,
) -> Json<Value> {
    let Some(db) = state.db.as_ref() else {
        return Json(json!({ "logs": [] }));
    };
    let guard = db.connection();
    let rows = match repos::recent_calls(&guard, query.limit()) {
        Ok(rows) => rows,
        Err(error) => return Json(json!({ "error": error.to_string(), "logs": [] })),
    };

    let logs: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "timestamp": row.timestamp,
                "model": row.model,
                "requested_model": row.requested_model,
                "provider": row.provider,
                "status": row.status,
                "duration_ms": row.duration_ms,
                "tokens_input": row.tokens_input,
                "tokens_output": row.tokens_output,
                "combo_name": row.combo_name,
                "error_summary": row.error_summary,
            })
        })
        .collect();

    Json(json!({ "logs": logs }))
}
