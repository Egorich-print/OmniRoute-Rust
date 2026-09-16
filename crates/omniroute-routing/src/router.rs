//! Model-prefix routing rules.
//!
//! A [`Router`] maps a model name to the ordered backend names to try.
//! Matching is case-insensitive prefix matching; the first matching rule
//! wins, otherwise the default chain applies. The caller guarantees the
//! ultimate fallback is present — an empty chain means "no backend".

/// One prefix rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteRule {
    /// Lowercase model prefix, e.g. `"grok-"`.
    pub model_prefix: String,
    /// Backend names to try in order.
    pub backends: Vec<String>,
}

/// Ordered routing table.
#[derive(Debug, Clone, Default)]
pub struct Router {
    rules: Vec<RouteRule>,
    default_backends: Vec<String>,
}

impl Router {
    /// Build a router. Prefixes are lowercased once, up front.
    pub fn new(rules: Vec<RouteRule>, default_backends: Vec<String>) -> Self {
        let rules = rules
            .into_iter()
            .map(|rule| RouteRule {
                model_prefix: rule.model_prefix.to_lowercase(),
                backends: rule.backends,
            })
            .collect();
        Self {
            rules,
            default_backends,
        }
    }

    /// Ordered backend names to try for `model`.
    pub fn route(&self, model: &str) -> Vec<String> {
        let lower = model.to_lowercase();
        for rule in &self.rules {
            if lower.starts_with(rule.model_prefix.as_str()) {
                return rule.backends.clone();
            }
        }
        self.default_backends.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn router() -> Router {
        Router::new(
            vec![RouteRule {
                model_prefix: "grok-".to_string(),
                backends: vec!["grok-cli".to_string(), "echo".to_string()],
            }],
            vec!["echo".to_string()],
        )
    }

    #[test]
    fn prefix_rule_wins_case_insensitively() {
        assert_eq!(
            router().route("Grok-4.6"),
            vec!["grok-cli".to_string(), "echo".to_string()]
        );
    }

    #[test]
    fn unmatched_model_uses_default() {
        assert_eq!(router().route("gpt-4o"), vec!["echo".to_string()]);
    }

    #[test]
    fn empty_router_routes_nowhere() {
        let router = Router::new(vec![], vec![]);
        assert!(router.route("anything").is_empty());
    }
}
