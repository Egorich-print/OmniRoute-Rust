//! Process environment for the Rust gateway.
//!
//! Deliberately narrow: the JS server reads ~700 variables, but the gateway
//! only needs a handful. Everything else stays in `key_value` settings.

use std::collections::HashMap;
use std::path::PathBuf;

/// Gateway configuration sourced from the environment.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    /// TCP port to listen on (`OMNIROUTE_RUST_PORT`, default 20129).
    pub port: u16,
    /// Data directory (`DATA_DIR`, else the default resolution).
    pub data_dir: PathBuf,
    /// Log filter (`RUST_LOG` / `OMNIROUTE_LOG`, default `"info"`).
    pub log_filter: String,
    /// Force API-key auth even when the `api_keys` table is empty
    /// (`OMNIROUTE_REQUIRE_AUTH=1`). When false, auth is still enforced as
    /// soon as at least one key exists.
    pub require_auth: bool,
}

impl GatewayConfig {
    /// Read configuration from the process environment.
    pub fn from_env() -> Self {
        let vars: HashMap<String, String> = std::env::vars().collect();
        Self::from_map(&vars)
    }

    /// Build configuration from an explicit map (tests, embeddings).
    pub fn from_map(vars: &HashMap<String, String>) -> Self {
        let get = |key: &str| vars.get(key).map(String::as_str).unwrap_or("");
        Self {
            port: get("OMNIROUTE_RUST_PORT").parse().unwrap_or(20_129),
            data_dir: if get("DATA_DIR").trim().is_empty() {
                omniroute_db::Db::data_dir()
            } else {
                PathBuf::from(get("DATA_DIR"))
            },
            log_filter: if get("RUST_LOG").is_empty() {
                if get("OMNIROUTE_LOG").is_empty() {
                    "info".to_string()
                } else {
                    get("OMNIROUTE_LOG").to_string()
                }
            } else {
                get("RUST_LOG").to_string()
            },
            require_auth: matches!(get("OMNIROUTE_REQUIRE_AUTH"), "1" | "true" | "yes"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn defaults_are_sane() {
        let config = GatewayConfig::from_map(&vars(&[]));
        assert_eq!(config.port, 20_129);
        assert_eq!(config.log_filter, "info");
        assert!(!config.require_auth);
    }

    #[test]
    fn port_flag_and_filter_parse() {
        let config = GatewayConfig::from_map(&vars(&[
            ("OMNIROUTE_RUST_PORT", "22000"),
            ("OMNIROUTE_REQUIRE_AUTH", "1"),
            ("RUST_LOG", "debug"),
        ]));
        assert_eq!(config.port, 22000);
        assert!(config.require_auth);
        assert_eq!(config.log_filter, "debug");
    }

    #[test]
    fn bad_port_falls_back_to_default() {
        let config = GatewayConfig::from_map(&vars(&[("OMNIROUTE_RUST_PORT", "nope")]));
        assert_eq!(config.port, 20_129);
    }
}
