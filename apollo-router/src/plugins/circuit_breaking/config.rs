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

/// Per-subgraph circuit breaker configuration.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct CircuitBreakerConfig {
    /// Whether circuit breaking is enabled.
    pub(crate) enabled: bool,
    /// Number of errors within the sliding window required to trip the circuit.
    pub(crate) error_threshold: u32,
    /// Duration of the sliding error-counting window.
    #[serde(with = "humantime_serde")]
    #[schemars(with = "String")]
    pub(crate) window: Duration,
    /// How long the circuit stays open before transitioning to half-open.
    #[serde(with = "humantime_serde")]
    #[schemars(with = "String")]
    pub(crate) recovery_timeout: Duration,
    /// Maximum concurrent probe requests allowed in the half-open state.
    pub(crate) half_open_max_requests: u32,
    /// Whether to enforce (reject requests) or just measure (log only).
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

/// Top-level plugin configuration with `all` + per-subgraph overrides.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
#[schemars(rename = "CircuitBreakingConfig")]
pub(crate) struct Config {
    /// Default circuit breaker settings applied to all subgraphs.
    all: Option<CircuitBreakerConfig>,
    /// Per-subgraph circuit breaker overrides.
    subgraphs: HashMap<String, CircuitBreakerConfig>,
}

impl Config {
    pub(crate) fn effective_config(&self, subgraph_name: &str) -> CircuitBreakerConfig {
        match (self.subgraphs.get(subgraph_name), &self.all) {
            (Some(specific), Some(global)) => merge_config(specific, global),
            (Some(specific), None) => specific.clone(),
            (None, Some(global)) => global.clone(),
            (None, None) => CircuitBreakerConfig::default(),
        }
    }
}

/// Merge a subgraph-specific config with the global fallback. Fields that are
/// at their default value in the specific config inherit from the global.
fn merge_config(
    specific: &CircuitBreakerConfig,
    global: &CircuitBreakerConfig,
) -> CircuitBreakerConfig {
    let default = CircuitBreakerConfig::default();
    CircuitBreakerConfig {
        enabled: specific.enabled,
        error_threshold: if specific.error_threshold != default.error_threshold {
            specific.error_threshold
        } else {
            global.error_threshold
        },
        window: if specific.window != default.window {
            specific.window
        } else {
            global.window
        },
        recovery_timeout: if specific.recovery_timeout != default.recovery_timeout {
            specific.recovery_timeout
        } else {
            global.recovery_timeout
        },
        half_open_max_requests: if specific.half_open_max_requests != default.half_open_max_requests
        {
            specific.half_open_max_requests
        } else {
            global.half_open_max_requests
        },
        mode: specific.mode,
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
        assert!(all.enabled);
        assert_eq!(all.error_threshold, 10);
        assert_eq!(all.window, Duration::from_secs(60));
        assert_eq!(all.recovery_timeout, Duration::from_secs(120));
        assert_eq!(all.half_open_max_requests, 2);
        assert_eq!(all.mode, CircuitBreakerMode::Measure);

        let products = config.subgraphs.get("products").unwrap();
        assert!(products.enabled);
        assert_eq!(products.error_threshold, 3);
        assert_eq!(products.mode, CircuitBreakerMode::Enforce);
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
                    "enabled": true,
                    "error_threshold": 3,
                    "recovery_timeout": "30s",
                }
            }
        }))
        .unwrap();

        let effective = config.effective_config("products");
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
}
