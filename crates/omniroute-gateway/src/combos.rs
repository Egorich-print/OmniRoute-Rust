//! DB-driven combo expansion.
//!
//! Ports the first slice of combo routing (`open-sse/services/combo.ts`):
//! a combo name expands to its ordered model refs (`"provider/model"`),
//! each resolved to a concrete `(backend, bare_model)` the gateway can
//! serve. Strategies (`priority` vs `auto`) both follow listed order for
//! now; scored auto-routing is deferred. Refs whose provider needs a
//! backend or format we do not serve yet are skipped, never fabricated.

use std::collections::{HashMap, HashSet};

use omniroute_providers::ProviderRegistry;
use rusqlite::Connection;
use serde_json::Value;

/// One combo entry: a provider plus its bare model id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComboEntry {
    /// Provider id, e.g. `"grok-cli"`.
    pub provider: String,
    /// Bare model id with the provider prefix stripped, e.g. `"grok-4.6"`.
    pub model: String,
}

/// A parsed combo: ordered entries to try in sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComboDef {
    /// Combo name as requested by clients.
    pub name: String,
    /// Strategy as stored (`"priority"`, `"auto"`); order is honoured for both.
    pub strategy: String,
    /// Ordered entries.
    pub entries: Vec<ComboEntry>,
}

/// Split a `"provider/model"` ref. A bare `"model"` yields an empty provider
/// and the id unchanged.
fn split_model_ref(model_ref: &str) -> (String, String) {
    match model_ref.split_once('/') {
        Some((provider, rest)) => (provider.to_string(), rest.to_string()),
        None => (String::new(), model_ref.to_string()),
    }
}

/// Parse one combo row (`name`, `data` JSON) into a [`ComboDef`].
///
/// Returns `None` when the row carries no usable model entries.
pub fn parse_combo(name: &str, data: &Value) -> Option<ComboDef> {
    let models = data.get("models")?.as_array()?;
    let mut entries = Vec::new();
    for item in models {
        if item
            .get("kind")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind != "model")
        {
            continue;
        }
        let model_ref = item.get("model").and_then(Value::as_str).unwrap_or("");
        if model_ref.is_empty() {
            continue;
        }
        let (split_provider, bare) = split_model_ref(model_ref);
        let provider = item
            .get("providerId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let provider = if provider.is_empty() {
            split_provider
        } else {
            provider.to_string()
        };
        if provider.is_empty() || bare.is_empty() {
            continue;
        }
        entries.push(ComboEntry {
            provider,
            model: bare,
        });
    }
    if entries.is_empty() {
        return None;
    }
    Some(ComboDef {
        name: name.to_string(),
        strategy: data
            .get("strategy")
            .and_then(Value::as_str)
            .unwrap_or("priority")
            .to_string(),
        entries,
    })
}

/// Load every parseable combo, keyed by name.
pub fn load_combos(conn: &Connection) -> HashMap<String, ComboDef> {
    let mut map = HashMap::new();
    let mut statement = match conn.prepare("SELECT name, data FROM combos") {
        Ok(statement) => statement,
        Err(_) => return map,
    };
    let rows = match statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }) {
        Ok(rows) => rows,
        Err(_) => return map,
    };
    for row in rows.flatten() {
        let (name, data) = row;
        if let Ok(json) = serde_json::from_str::<Value>(&data)
            && let Some(combo) = parse_combo(&name, &json)
        {
            map.insert(name, combo);
        }
    }
    map
}

/// Resolve a combo to ordered `(backend, model)` steps.
///
/// `available` holds the backend names the gateway actually runs. A ref maps
/// to its own provider backend when one is configured, then to `grok-cli`
/// for that provider, then to a shared `openai-compatible` endpoint when the
/// registry reports an OpenAI wire format. Anything else is skipped.
pub fn plan_steps(
    combo: &ComboDef,
    registry: &ProviderRegistry,
    available: &HashSet<String>,
) -> Vec<(String, String)> {
    let mut steps = Vec::new();
    for entry in &combo.entries {
        // A backend named after the provider serves it directly.
        if available.contains(entry.provider.as_str()) {
            steps.push((entry.provider.clone(), entry.model.clone()));
            continue;
        }
        if entry.provider == "grok-cli" && available.contains("grok-cli") {
            steps.push(("grok-cli".to_string(), entry.model.clone()));
            continue;
        }
        if registry.is_openai_format(&entry.provider) && available.contains("openai-compatible") {
            steps.push(("openai-compatible".to_string(), entry.model.clone()));
        }
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn combo_json() -> Value {
        json!({
            "strategy": "priority",
            "models": [
                {"kind": "model", "model": "grok-cli/grok-4.6", "providerId": "grok-cli"},
                {"kind": "model", "model": "openrouter/anthropic/claude-3-haiku", "providerId": "openrouter"},
                {"kind": "other", "model": "x/y"},
                {"kind": "model", "model": ""},
            ],
        })
    }

    #[test]
    fn parses_entries_and_skips_unusable() {
        let combo = parse_combo("c", &combo_json()).unwrap();
        assert_eq!(combo.strategy, "priority");
        assert_eq!(combo.entries.len(), 2);
        assert_eq!(
            combo.entries[0],
            ComboEntry {
                provider: "grok-cli".to_string(),
                model: "grok-4.6".to_string(),
            }
        );
        assert_eq!(
            combo.entries[1],
            ComboEntry {
                provider: "openrouter".to_string(),
                model: "anthropic/claude-3-haiku".to_string(),
            }
        );
    }

    #[test]
    fn empty_combos_parse_to_none() {
        assert_eq!(parse_combo("c", &json!({})), None);
        assert_eq!(parse_combo("c", &json!({"models": []})), None);
    }

    #[test]
    fn plans_prefer_grok_cli_then_openai_compat() {
        let registry = ProviderRegistry::load();
        let combo = parse_combo("c", &combo_json()).unwrap();
        let available: HashSet<String> = ["grok-cli", "openai-compatible", "echo"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let steps = plan_steps(&combo, &registry, &available);
        assert_eq!(
            steps,
            vec![
                ("grok-cli".to_string(), "grok-4.6".to_string()),
                (
                    "openai-compatible".to_string(),
                    "anthropic/claude-3-haiku".to_string()
                ),
            ]
        );
    }

    #[test]
    fn plans_skip_missing_backends() {
        let registry = ProviderRegistry::load();
        let combo = parse_combo("c", &combo_json()).unwrap();
        let available: HashSet<String> = ["echo"].into_iter().map(str::to_string).collect();
        assert!(plan_steps(&combo, &registry, &available).is_empty());
    }
}
