//! Admin API backing the dashboard.
//!
//! Serves what the JS dashboard showed, over the same SQLite the gateway
//! already uses: which providers are live, how combos resolve, recent usage
//! and recent calls. Secrets are never serialised — only whether a credential
//! exists.
//!
//! Mutations stay deliberately small: renaming / reordering / hiding combos
//! and toggling connection activity or priority. Credential rotation is not
//! exposed here; it stays a JS-dashboard/CLI operation until the Rust side
//! owns decryption.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use omniroute_db::repos;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::auth::Authenticated;
use crate::combos::{dispatch_backend, parse_combo_rich};

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
    let mut out: Vec<Value> = Vec::new();

    if let Ok(rows) = repos::list_combos(&guard) {
        for row in rows {
            let data: Value = serde_json::from_str(&row.data).unwrap_or(Value::Null);
            let (strategy, rich) = parse_combo_rich(&data).unwrap_or_default();
            let entries: Vec<Value> = rich
                .iter()
                .map(|item| {
                    let backend = dispatch_backend(&item.entry, registry, &available);
                    json!({
                        "id": item.id,
                        "provider": item.entry.provider,
                        "model": item.entry.model,
                        "weight": item.weight,
                        "backend": backend,
                        "servable": backend.is_some(),
                    })
                })
                .collect();
            out.push(json!({
                "id": row.id,
                "name": row.name,
                "strategy": strategy,
                "sort_order": row.sort_order,
                "is_hidden": is_hidden(&row.data),
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

/// Read the `isHidden` flag out of a combo `data` blob.
///
/// A corrupt or non-object blob counts as visible: hiding must be explicit.
fn is_hidden(data: &str) -> bool {
    serde_json::from_str::<Value>(data)
        .ok()
        .and_then(|value| value.get("isHidden")?.as_bool())
        .unwrap_or(false)
}

/// Body for `PATCH /api/combos/:id`. Every field is optional; absent fields
/// are left untouched.
#[derive(Debug, Deserialize)]
pub struct UpdateComboBody {
    pub name: Option<String>,
    pub sort_order: Option<i64>,
    pub is_hidden: Option<bool>,
    pub strategy: Option<String>,
    pub models: Option<Vec<ComboModelBody>>,
}

/// One model entry in a combo rewrite: provider plus bare model id.
///
/// `weight` defaults to 50 when absent. Entry `id` is kept when sent back
/// (the dashboard round-trips it); otherwise the server derives one.
#[derive(Debug, Deserialize)]
pub struct ComboModelBody {
    pub id: Option<String>,
    pub provider: String,
    pub model: String,
    pub weight: Option<i64>,
}

/// Strategies the gateway understands. Anything else is a 400: storing an
/// unknown strategy would silently change routing semantics.
fn is_known_strategy(strategy: &str) -> bool {
    matches!(strategy, "priority" | "auto")
}

/// Body for `PATCH /api/connections/:id`. Every field is optional; absent
/// fields are left untouched.
#[derive(Debug, Deserialize)]
pub struct UpdateConnectionBody {
    pub name: Option<String>,
    pub is_active: Option<bool>,
    pub priority: Option<i64>,
}

/// A SQLite unique-constraint failure (duplicate combo name on rename).
fn is_unique_violation(error: &omniroute_db::DbError) -> bool {
    error.to_string().contains("UNIQUE constraint failed")
}

/// `PATCH /api/combos/:id` — rename, reorder, hide, restyle or restock a combo.
///
/// Combo routing reads the database on every request, so the change takes
/// effect immediately. `404` when `id` is unknown, `409` when `name` collides
/// with another combo, `400` when the body carries nothing usable, names an
/// unknown strategy, or sends an empty/unusable model list.
pub async fn update_combo(
    State(state): State<AppState>,
    _auth: Authenticated,
    Path(id): Path<String>,
    Json(body): Json<UpdateComboBody>,
) -> impl IntoResponse {
    if let Some(name) = body.name.as_deref()
        && name.trim().is_empty()
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "name must not be empty" })),
        );
    }
    if let Some(strategy) = body.strategy.as_deref()
        && !is_known_strategy(strategy)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "strategy must be \"priority\" or \"auto\"" })),
        );
    }
    let mut models: Option<Vec<repos::ComboModelSpec>> = None;
    if let Some(items) = body.models.as_deref() {
        if items.is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "models must not be empty" })),
            );
        }
        let mut specs = Vec::with_capacity(items.len());
        for item in items {
            if item.provider.trim().is_empty() || item.model.trim().is_empty() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "every model needs a provider and a model id" })),
                );
            }
            let weight = item.weight.unwrap_or(50);
            if !(0..=100).contains(&weight) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "weight must be between 0 and 100" })),
                );
            }
            specs.push(repos::ComboModelSpec {
                id: item.id.clone(),
                provider: item.provider.trim().to_string(),
                model: item.model.trim().to_string(),
                weight,
            });
        }
        models = Some(specs);
    }
    let patch = repos::ComboMetaPatch {
        name: body.name.map(|name| name.trim().to_string()),
        sort_order: body.sort_order,
        is_hidden: body.is_hidden,
        strategy: body.strategy,
        models,
    };
    if patch.name.is_none()
        && patch.sort_order.is_none()
        && patch.is_hidden.is_none()
        && patch.strategy.is_none()
        && patch.models.is_none()
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "no fields to update" })),
        );
    }
    let Some(db) = state.db.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "database not configured" })),
        );
    };
    let guard = db.connection();
    match repos::update_combo_meta(&guard, &id, &patch) {
        Ok(Some(row)) => (
            StatusCode::OK,
            Json(json!({
                "status": "ok",
                "id": row.id,
                "name": row.name,
                "sort_order": row.sort_order,
                "is_hidden": is_hidden(&row.data),
            })),
        ),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "combo not found" })),
        ),
        Err(error) if is_unique_violation(&error) => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "another combo already uses that name" })),
        ),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error.to_string() })),
        ),
    }
}

/// `PATCH /api/connections/:id` — rename, toggle or reprioritise a provider
/// connection.
///
/// `restart_required` is true when `is_active` flipped: backends are built
/// once at startup, so the live gateway only picks the change up on restart.
pub async fn update_connection(
    State(state): State<AppState>,
    _auth: Authenticated,
    Path(id): Path<String>,
    Json(body): Json<UpdateConnectionBody>,
) -> impl IntoResponse {
    if body.name.is_none() && body.is_active.is_none() && body.priority.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "no fields to update" })),
        );
    }
    let Some(db) = state.db.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "database not configured" })),
        );
    };
    let guard = db.connection();
    let previously_active = repos::get_connection(&guard, &id)
        .ok()
        .flatten()
        .map(|row| row.is_active);
    let patch = repos::ConnectionMetaPatch {
        name: body.name,
        is_active: body.is_active,
        priority: body.priority,
    };
    match repos::update_connection_meta(&guard, &id, &patch) {
        Ok(Some(row)) => {
            let restart_required = previously_active.is_some_and(|active| active != row.is_active);
            (
                StatusCode::OK,
                Json(json!({
                    "status": "ok",
                    "id": row.id,
                    "is_active": row.is_active,
                    "priority": row.priority,
                    "restart_required": restart_required,
                })),
            )
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "connection not found" })),
        ),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error.to_string() })),
        ),
    }
}
