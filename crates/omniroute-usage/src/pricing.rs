//! Model pricing in USD per 1M tokens.
//!
//! Resolution order mirrors the JS server: user overrides stored under the
//! `pricing` `key_value` namespace win, then the built-in seed below. Models
//! missing from both resolve to `None` — unknown cost is reported as unknown,
//! never fabricated.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Per-1M-token prices for one model.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModelPrice {
    pub input: f64,
    pub output: f64,
    #[serde(default)]
    pub cached: f64,
    #[serde(default)]
    pub reasoning: f64,
    #[serde(default)]
    pub cache_creation: f64,
}

/// Price catalog: built-in seed plus user overrides.
#[derive(Debug, Clone, Default)]
pub struct PriceTable {
    prices: HashMap<String, ModelPrice>,
}

impl PriceTable {
    /// Seed with the reference models (values verbatim from the JS catalog).
    pub fn with_defaults() -> Self {
        let mut table = Self::default();
        // xAI (frontier-labs.ts / inference-hosts.ts).
        table.insert(
            "grok-4.6",
            ModelPrice {
                input: 2.0,
                output: 6.0,
                cached: 0.5,
                reasoning: 6.0,
                cache_creation: 2.0,
            },
        );
        table.insert(
            "grok-4.5",
            ModelPrice {
                input: 1.4,
                output: 4.2,
                cached: 0.35,
                reasoning: 4.2,
                cache_creation: 1.4,
            },
        );
        // OpenAI (frontier-labs.ts).
        table.insert(
            "gpt-4o-mini",
            ModelPrice {
                input: 0.15,
                output: 0.6,
                cached: 0.075,
                reasoning: 0.9,
                cache_creation: 0.15,
            },
        );
        // Anthropic (frontier-labs.ts).
        table.insert(
            "claude-sonnet-4.6",
            ModelPrice {
                input: 3.0,
                output: 15.0,
                cached: 1.5,
                reasoning: 22.5,
                cache_creation: 3.0,
            },
        );
        // Google (frontier-labs.ts).
        table.insert(
            "gemini-2.5-flash",
            ModelPrice {
                input: 0.3,
                output: 2.5,
                cached: 0.03,
                reasoning: 3.75,
                cache_creation: 0.3,
            },
        );
        table
    }

    fn insert(&mut self, model: &str, price: ModelPrice) {
        self.prices.insert(model.to_string(), price);
    }

    /// Apply a user override (wins over the seed).
    pub fn set_override(&mut self, model: &str, price: ModelPrice) {
        self.insert(model, price);
    }

    /// Look up the price for a model.
    pub fn price_for(&self, model: &str) -> Option<&ModelPrice> {
        self.prices.get(model)
    }

    /// Cost in USD, mirroring `costCalculator.ts`: cached input is billed at
    /// the cached rate, reasoning pays the reasoning-minus-output delta.
    /// Returns `None` for unpriced models.
    pub fn cost_usd(
        &self,
        model: &str,
        input: u32,
        output: u32,
        cached: u32,
        reasoning: u32,
        cache_creation: u32,
    ) -> Option<f64> {
        let price = self.price_for(model)?;
        let non_cached = input.saturating_sub(cached) as f64;
        let total = non_cached * price.input
            + cached as f64 * price.cached
            + output as f64 * price.output
            + reasoning as f64 * (price.reasoning - price.output)
            + cache_creation as f64 * price.cache_creation;
        Some(total / 1_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grok_pricing_matches_js_catalog() {
        let table = PriceTable::with_defaults();
        let price = table.price_for("grok-4.6").unwrap();
        assert_eq!(price.input, 2.0);
        assert_eq!(price.output, 6.0);
    }

    #[test]
    fn cost_formula_matches_js() {
        let table = PriceTable::with_defaults();
        // 1000 input (200 cached), 500 output, 100 reasoning.
        let cost = table.cost_usd("grok-4.6", 1000, 500, 200, 100, 0).unwrap();
        let expected = (800.0 * 2.0 + 200.0 * 0.5 + 500.0 * 6.0 + 100.0 * (6.0 - 6.0)) / 1e6;
        assert!((cost - expected).abs() < 1e-12, "{cost} vs {expected}");
    }

    #[test]
    fn unknown_model_has_no_price() {
        let table = PriceTable::with_defaults();
        assert_eq!(table.price_for("nope-1"), None);
        assert_eq!(table.cost_usd("nope-1", 1, 1, 0, 0, 0), None);
    }

    #[test]
    fn override_wins_over_seed() {
        let mut table = PriceTable::with_defaults();
        table.set_override(
            "grok-4.6",
            ModelPrice {
                input: 1.0,
                output: 1.0,
                cached: 0.0,
                reasoning: 1.0,
                cache_creation: 0.0,
            },
        );
        assert_eq!(table.price_for("grok-4.6").unwrap().input, 1.0);
    }
}
