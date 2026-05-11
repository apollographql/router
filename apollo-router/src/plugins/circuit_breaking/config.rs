use std::collections::HashMap;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

const DEFAULT_ERROR_THRESHOLD: u32 = 5;
const DEFAULT_WINDOW: Duration = Duration::from_secs(30);
const DEFAULT_RECOVERY_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_HALF_OPEN_MAX_REQUESTS: u32 = 1;

/// Mode controlling whether the circuit breaker enforces or just observes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CircuitBreakerMode {
    /// Track and log state transitions but never reject requests.
    Measure,
    /// Actively reject requests when the circuit is open.
    #[default]
    Enforce,
}

/// Serde-facing circuit breaker configuration where every field is optional.
/// `None` means "not specified by the user" and will be inherited from the
/// parent scope (global `all`) or fall back to the built-in default.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct CircuitBreakerInput {
    /// Whether circuit breaking is enabled.
    pub(crate) enabled: Option<bool>,
    /// Number of errors within the sliding window required to trip the circuit.
    pub(crate) error_threshold: Option<u32>,
    /// Duration of the sliding error-counting window.
    #[serde(with = "humantime_serde")]
    #[schemars(with = "Option<String>")]
    pub(crate) window: Option<Duration>,
    /// How long the circuit stays open before transitioning to half-open.
    #[serde(with = "humantime_serde")]
    #[schemars(with = "Option<String>")]
    pub(crate) recovery_timeout: Option<Duration>,
    /// Maximum concurrent probe requests allowed in the half-open state.
    pub(crate) half_open_max_requests: Option<u32>,
    /// Whether to enforce (reject requests) or just measure (log only).
    pub(crate) mode: Option<CircuitBreakerMode>,
}

/// Fully-resolved circuit breaker configuration with concrete values.
/// Produced by merging a specific scope with its parent and applying defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CircuitBreakerConfig {
    pub(crate) enabled: bool,
    pub(crate) error_threshold: u32,
    pub(crate) window: Duration,
    pub(crate) recovery_timeout: Duration,
    pub(crate) half_open_max_requests: u32,
    pub(crate) mode: CircuitBreakerMode,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            error_threshold: DEFAULT_ERROR_THRESHOLD,
            window: DEFAULT_WINDOW,
            recovery_timeout: DEFAULT_RECOVERY_TIMEOUT,
            half_open_max_requests: DEFAULT_HALF_OPEN_MAX_REQUESTS,
            mode: CircuitBreakerMode::default(),
        }
    }
}

impl CircuitBreakerConfig {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.error_threshold == 0 {
            return Err("circuit_breaking: error_threshold must be greater than 0".into());
        }
        if self.half_open_max_requests == 0 {
            return Err(
                "circuit_breaking: half_open_max_requests must be greater than 0".into(),
            );
        }
        Ok(())
    }
}

/// Resolve a specific input against an optional parent, falling back to
/// built-in defaults for anything still unset.
fn resolve(specific: &CircuitBreakerInput, parent: Option<&CircuitBreakerInput>) -> CircuitBreakerConfig {
    CircuitBreakerConfig {
        enabled: specific
            .enabled
            .or(parent.and_then(|p| p.enabled))
            .unwrap_or(false),
        error_threshold: specific
            .error_threshold
            .or(parent.and_then(|p| p.error_threshold))
            .unwrap_or(DEFAULT_ERROR_THRESHOLD),
        window: specific
            .window
            .or(parent.and_then(|p| p.window))
            .unwrap_or(DEFAULT_WINDOW),
        recovery_timeout: specific
            .recovery_timeout
            .or(parent.and_then(|p| p.recovery_timeout))
            .unwrap_or(DEFAULT_RECOVERY_TIMEOUT),
        half_open_max_requests: specific
            .half_open_max_requests
            .or(parent.and_then(|p| p.half_open_max_requests))
            .unwrap_or(DEFAULT_HALF_OPEN_MAX_REQUESTS),
        mode: specific
            .mode
            .or(parent.and_then(|p| p.mode))
            .unwrap_or_default(),
    }
}

/// Circuit breaker configuration scoped to connector sources.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct ConnectorCircuitBreakerConfig {
    /// Default circuit breaker settings applied to all connector sources.
    all: Option<CircuitBreakerInput>,
    /// Per-source circuit breaker overrides, keyed by source config key
    /// (e.g. `"subgraph_name.source_name"`).
    sources: HashMap<String, CircuitBreakerInput>,
}

/// Top-level plugin configuration with `all` + per-subgraph overrides.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
#[schemars(rename = "CircuitBreakingConfig")]
pub(crate) struct Config {
    /// Default circuit breaker settings applied to all subgraphs.
    all: Option<CircuitBreakerInput>,
    /// Per-subgraph circuit breaker overrides.
    subgraphs: HashMap<String, CircuitBreakerInput>,
    /// Circuit breaker settings for connector sources.
    connector: ConnectorCircuitBreakerConfig,
}

impl Config {
    pub(crate) fn effective_config(&self, subgraph_name: &str) -> CircuitBreakerConfig {
        match (self.subgraphs.get(subgraph_name), &self.all) {
            (Some(specific), Some(global)) => resolve(specific, Some(global)),
            (Some(specific), None) => resolve(specific, None),
            (None, Some(global)) => resolve(global, None),
            (None, None) => CircuitBreakerConfig::default(),
        }
    }

    pub(crate) fn effective_connector_config(&self, source_name: &str) -> CircuitBreakerConfig {
        match (
            self.connector.sources.get(source_name),
            &self.connector.all,
        ) {
            (Some(specific), Some(global)) => resolve(specific, Some(global)),
            (Some(specific), None) => resolve(specific, None),
            (None, Some(global)) => resolve(global, None),
            (None, None) => CircuitBreakerConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn deserialize_default_config() {
        let config: Config = serde_json::from_value(json!({})).unwrap();
        assert!(config.all.is_none());
        assert!(config.subgraphs.is_empty());
    }

    #[test]
    fn deserialize_full_config() {
        let config: Config = serde_json::from_value(json!({
            "all": {
                "enabled": true,
                "error_threshold": 10,
                "window": "60s",
                "recovery_timeout": "120s",
                "half_open_max_requests": 2,
                "mode": "measure"
            },
            "subgraphs": {
                "products": {
                    "enabled": true,
                    "error_threshold": 3,
                    "window": "15s",
                    "recovery_timeout": "30s",
                    "half_open_max_requests": 1,
                    "mode": "enforce"
                }
            }
        }))
        .unwrap();

        let all = config.all.as_ref().unwrap();
        assert_eq!(all.enabled, Some(true));
        assert_eq!(all.error_threshold, Some(10));
        assert_eq!(all.window, Some(Duration::from_secs(60)));
        assert_eq!(all.recovery_timeout, Some(Duration::from_secs(120)));
        assert_eq!(all.half_open_max_requests, Some(2));
        assert_eq!(all.mode, Some(CircuitBreakerMode::Measure));

        let products = config.subgraphs.get("products").unwrap();
        assert_eq!(products.enabled, Some(true));
        assert_eq!(products.error_threshold, Some(3));
        assert_eq!(products.mode, Some(CircuitBreakerMode::Enforce));
    }

    #[test]
    fn effective_config_uses_global_when_no_subgraph_override() {
        let config: Config = serde_json::from_value(json!({
            "all": {
                "enabled": true,
                "error_threshold": 10,
                "window": "60s",
                "recovery_timeout": "120s",
                "mode": "measure"
            }
        }))
        .unwrap();

        let effective = config.effective_config("any_subgraph");
        assert!(effective.enabled);
        assert_eq!(effective.error_threshold, 10);
        assert_eq!(effective.window, Duration::from_secs(60));
        assert_eq!(effective.recovery_timeout, Duration::from_secs(120));
        assert_eq!(effective.mode, CircuitBreakerMode::Measure);
    }

    #[test]
    fn effective_config_subgraph_overrides_global() {
        let config: Config = serde_json::from_value(json!({
            "all": {
                "enabled": true,
                "error_threshold": 10,
                "window": "60s",
                "recovery_timeout": "120s",
            },
            "subgraphs": {
                "products": {
                    "error_threshold": 3,
                    "recovery_timeout": "30s",
                }
            }
        }))
        .unwrap();

        let effective = config.effective_config("products");
        // enabled inherits from global
        assert!(effective.enabled);
        assert_eq!(effective.error_threshold, 3);
        // window inherits from global since products didn't override it
        assert_eq!(effective.window, Duration::from_secs(60));
        assert_eq!(effective.recovery_timeout, Duration::from_secs(30));
    }

    #[test]
    fn effective_config_defaults_when_nothing_configured() {
        let config: Config = serde_json::from_value(json!({})).unwrap();
        let effective = config.effective_config("anything");
        assert!(!effective.enabled);
        assert_eq!(effective.error_threshold, DEFAULT_ERROR_THRESHOLD);
        assert_eq!(effective.window, DEFAULT_WINDOW);
        assert_eq!(effective.recovery_timeout, DEFAULT_RECOVERY_TIMEOUT);
    }

    #[test]
    fn rejects_unknown_fields() {
        let result: Result<Config, _> = serde_json::from_value(json!({
            "all": {
                "enabled": true,
                "bogus_field": 123
            }
        }));
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_connector_config() {
        let config: Config = serde_json::from_value(json!({
            "connector": {
                "all": {
                    "enabled": true,
                    "error_threshold": 3,
                    "window": "30s",
                    "mode": "enforce"
                },
                "sources": {
                    "connectors.jsonPlaceholder": {
                        "enabled": true,
                        "error_threshold": 10,
                        "window": "15s"
                    }
                }
            }
        }))
        .unwrap();

        let all = config.connector.all.as_ref().unwrap();
        assert_eq!(all.enabled, Some(true));
        assert_eq!(all.error_threshold, Some(3));
        assert_eq!(all.window, Some(Duration::from_secs(30)));

        let source = config
            .connector
            .sources
            .get("connectors.jsonPlaceholder")
            .unwrap();
        assert_eq!(source.enabled, Some(true));
        assert_eq!(source.error_threshold, Some(10));
        assert_eq!(source.window, Some(Duration::from_secs(15)));
    }

    #[test]
    fn effective_connector_config_uses_all_when_no_source_override() {
        let config: Config = serde_json::from_value(json!({
            "connector": {
                "all": {
                    "enabled": true,
                    "error_threshold": 3,
                    "window": "30s",
                    "recovery_timeout": "90s",
                    "mode": "measure"
                }
            }
        }))
        .unwrap();

        let effective = config.effective_connector_config("any_source");
        assert!(effective.enabled);
        assert_eq!(effective.error_threshold, 3);
        assert_eq!(effective.window, Duration::from_secs(30));
        assert_eq!(effective.recovery_timeout, Duration::from_secs(90));
        assert_eq!(effective.mode, CircuitBreakerMode::Measure);
    }

    #[test]
    fn effective_connector_config_source_overrides_all() {
        let config: Config = serde_json::from_value(json!({
            "connector": {
                "all": {
                    "enabled": true,
                    "error_threshold": 10,
                    "window": "60s",
                    "recovery_timeout": "120s"
                },
                "sources": {
                    "connectors.jsonPlaceholder": {
                        "error_threshold": 3,
                        "recovery_timeout": "30s"
                    }
                }
            }
        }))
        .unwrap();

        let effective = config.effective_connector_config("connectors.jsonPlaceholder");
        // enabled inherits from connector.all
        assert!(effective.enabled);
        assert_eq!(effective.error_threshold, 3);
        // window inherits from connector.all
        assert_eq!(effective.window, Duration::from_secs(60));
        assert_eq!(effective.recovery_timeout, Duration::from_secs(30));
    }

    #[test]
    fn effective_connector_config_defaults_when_nothing_configured() {
        let config: Config = serde_json::from_value(json!({})).unwrap();
        let effective = config.effective_connector_config("anything");
        assert!(!effective.enabled);
        assert_eq!(effective.error_threshold, DEFAULT_ERROR_THRESHOLD);
        assert_eq!(effective.window, DEFAULT_WINDOW);
        assert_eq!(effective.recovery_timeout, DEFAULT_RECOVERY_TIMEOUT);
    }

    #[test]
    fn subgraph_and_connector_configs_are_independent() {
        let config: Config = serde_json::from_value(json!({
            "all": {
                "enabled": true,
                "error_threshold": 5
            },
            "connector": {
                "all": {
                    "enabled": true,
                    "error_threshold": 3
                }
            }
        }))
        .unwrap();

        let subgraph_effective = config.effective_config("products");
        assert_eq!(subgraph_effective.error_threshold, 5);

        let connector_effective = config.effective_connector_config("connectors.jsonPlaceholder");
        assert_eq!(connector_effective.error_threshold, 3);
    }

    #[test]
    fn enabled_inherits_from_global() {
        let config: Config = serde_json::from_value(json!({
            "all": {
                "enabled": true,
                "error_threshold": 10
            },
            "subgraphs": {
                "products": {
                    "error_threshold": 3
                }
            }
        }))
        .unwrap();

        let effective = config.effective_config("products");
        assert!(effective.enabled, "enabled should inherit from global all");
        assert_eq!(effective.error_threshold, 3);
    }

    #[test]
    fn mode_inherits_from_global() {
        let config: Config = serde_json::from_value(json!({
            "all": {
                "enabled": true,
                "mode": "measure"
            },
            "subgraphs": {
                "products": {
                    "error_threshold": 3
                }
            }
        }))
        .unwrap();

        let effective = config.effective_config("products");
        assert_eq!(
            effective.mode,
            CircuitBreakerMode::Measure,
            "mode should inherit from global all"
        );
    }

    #[test]
    fn explicit_default_value_is_respected() {
        let config: Config = serde_json::from_value(json!({
            "all": {
                "enabled": true,
                "error_threshold": 10
            },
            "subgraphs": {
                "products": {
                    "error_threshold": 5
                }
            }
        }))
        .unwrap();

        let effective = config.effective_config("products");
        assert_eq!(
            effective.error_threshold, 5,
            "explicit threshold of 5 (the default) should be used, not inherited 10"
        );
    }

    #[test]
    fn validate_rejects_zero_error_threshold() {
        let config = CircuitBreakerConfig {
            error_threshold: 0,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_rejects_zero_half_open_max_requests() {
        let config = CircuitBreakerConfig {
            half_open_max_requests: 0,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_accepts_valid_config() {
        let config = CircuitBreakerConfig::default();
        assert!(config.validate().is_ok());
    }
}
