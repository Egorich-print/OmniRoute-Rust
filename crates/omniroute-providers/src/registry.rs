//! Static provider registry derived from the JS `REGISTRY`.

use std::collections::HashMap;

use omniroute_core::WireFormat;
use serde::{Deserialize, Serialize};

/// One model entry. `target_format` is `None` when the model inherits the
/// provider's `format` (the same fallback `getTargetFormat` applies in JS).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryModel {
    pub id: String,
    pub target_format: Option<String>,
}

/// One provider entry. Function fields of the JS `RegistryEntry`
/// (`urlBuilder`, header profiles) are intentionally not carried here — they
/// are ported as code per executor, not as data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderEntry {
    pub id: String,
    pub alias: Option<String>,
    pub format: Option<String>,
    pub executor: Option<String>,
    pub auth_type: Option<String>,
    #[serde(default)]
    pub models: Vec<RegistryModel>,
}

/// Map a registry format string to the core wire format.
///
/// Returns `None` for formats the core does not model yet (`clova`, `codex`,
/// `kiro`, `cursor`, …) — callers must handle those explicitly rather than
/// guessing.
pub fn parse_wire_format(value: &str) -> Option<WireFormat> {
    match value {
        "openai" => Some(WireFormat::OpenAi),
        "openai-responses" => Some(WireFormat::OpenAiResponses),
        "claude" => Some(WireFormat::Claude),
        "gemini" => Some(WireFormat::Gemini),
        "antigravity" => Some(WireFormat::Antigravity),
        _ => None,
    }
}

/// The embedded provider registry.
#[derive(Debug, Clone)]
pub struct ProviderRegistry {
    entries: Vec<ProviderEntry>,
    by_id: HashMap<String, usize>,
}

impl ProviderRegistry {
    /// Load the registry embedded at compile time.
    ///
    /// # Panics
    ///
    /// Panics if the embedded `registry.json` is malformed. The file is
    /// generated from the JS source and validated by the contract tests, so a
    /// malformed file is a build-time bug, not a runtime condition.
    pub fn load() -> Self {
        let entries: Vec<ProviderEntry> =
            serde_json::from_str(include_str!("../registry.json")).expect("registry.json valid");
        Self::from_entries(entries)
    }

    fn from_entries(entries: Vec<ProviderEntry>) -> Self {
        let by_id = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.id.clone(), index))
            .collect();
        Self { entries, by_id }
    }

    /// Number of registered providers.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over all entries in registry order.
    pub fn iter(&self) -> impl Iterator<Item = &ProviderEntry> {
        self.entries.iter()
    }

    /// Iterate over all provider ids in registry order.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|entry| entry.id.as_str())
    }

    /// Look up a provider by id.
    pub fn get(&self, id: &str) -> Option<&ProviderEntry> {
        self.by_id
            .get(id)
            .and_then(|index| self.entries.get(*index))
    }

    /// Effective target format for a model: the model's explicit
    /// `target_format`, falling back to the provider `format`.
    pub fn model_target_format(&self, provider_id: &str, model_id: &str) -> Option<&str> {
        let provider = self.get(provider_id)?;
        let model = provider.models.iter().find(|m| m.id == model_id)?;
        model
            .target_format
            .as_deref()
            .or(provider.format.as_deref())
    }

    /// The effective target format as a core wire format, if it is modelled.
    pub fn model_wire_format(&self, provider_id: &str, model_id: &str) -> Option<WireFormat> {
        self.model_target_format(provider_id, model_id)
            .and_then(parse_wire_format)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn registry() -> ProviderRegistry {
        ProviderRegistry::load()
    }

    #[test]
    fn registry_loads_and_is_nontrivial() {
        let registry = registry();
        assert!(registry.len() >= 200, "len = {}", registry.len());
    }

    #[test]
    fn provider_ids_are_unique() {
        let registry = registry();
        let mut seen = HashSet::new();
        for id in registry.ids() {
            assert!(seen.insert(id), "duplicate provider id: {id}");
        }
    }

    #[test]
    fn every_provider_has_executor_and_format() {
        for entry in registry().iter() {
            assert!(
                entry.executor.as_deref().is_some_and(|s| !s.is_empty()),
                "missing executor: {}",
                entry.id
            );
            assert!(
                entry.format.as_deref().is_some_and(|s| !s.is_empty()),
                "missing format: {}",
                entry.id
            );
        }
    }

    #[test]
    fn every_model_has_an_id() {
        for entry in registry().iter() {
            for model in &entry.models {
                assert!(!model.id.is_empty(), "empty model id under {}", entry.id);
            }
        }
    }

    #[test]
    fn grok_cli_model_uses_responses_format() {
        let registry = registry();
        assert_eq!(
            registry.model_target_format("grok-cli", "grok-4.6"),
            Some("openai-responses")
        );
        assert_eq!(
            registry.model_wire_format("grok-cli", "grok-4.6"),
            Some(WireFormat::OpenAiResponses)
        );
    }

    #[test]
    fn model_without_explicit_target_inherits_provider_format() {
        let registry = registry();
        assert_eq!(
            registry.model_target_format("adapta-web", "adapta-one"),
            Some("openai")
        );
    }

    #[test]
    fn unknown_provider_or_model_returns_none() {
        let registry = registry();
        assert_eq!(registry.model_target_format("nope", "x"), None);
        assert_eq!(registry.model_target_format("grok-cli", "nope"), None);
    }

    #[test]
    fn unmodelled_formats_return_none() {
        assert_eq!(parse_wire_format("clova"), None);
        assert_eq!(parse_wire_format("kiro"), None);
        assert_eq!(parse_wire_format(""), None);
    }

    #[test]
    fn provider_id_list_snapshot() {
        let registry = registry();
        let ids: Vec<&str> = registry.ids().collect();
        insta::assert_debug_snapshot!(ids);
    }
}
