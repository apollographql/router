use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

use crate::configuration::mode::Mode;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct CooperativeCancellation {
    /// When true, cooperative cancellation is enabled.
    enabled: bool,
    /// When enabled, this sets whether the router will cancel query planning or
    /// merely emit a metric when it would have happened.
    mode: Mode,
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[serde(serialize_with = "humantime_serde::serialize")]
    #[schemars(with = "Option<String>")]
    /// Enable timeout for query planning.
    timeout: Option<Duration>,
}

impl Default for CooperativeCancellation {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: Mode::Measure,
            timeout: None,
        }
    }
}

impl CooperativeCancellation {
    /// Returns the timeout, if configured.
    pub(crate) fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    #[cfg(test)]
    /// Create a new `CooperativeCancellation` config in enforcement mode.
    pub(crate) fn enabled() -> Self {
        Self {
            enabled: true,
            mode: Mode::Enforce,
            timeout: None,
        }
    }

    /// Returns true if cooperative cancellation is enabled.
    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Returns true if this config is in measure mode.
    pub(crate) fn is_measure_mode(&self) -> bool {
        self.mode.is_measure_mode()
    }

    /// Returns true if this config is in enforce mode.
    pub(crate) fn is_enforce_mode(&self) -> bool {
        self.mode.is_enforce_mode()
    }

    #[cfg(test)]
    /// Create a new `CooperativeCancellation` config in enforcement mode with a timeout.
    pub(crate) fn enabled_with_timeout(timeout: Duration) -> Self {
        Self {
            enabled: true,
            mode: Mode::Enforce,
            timeout: Some(timeout),
        }
    }

    #[cfg(test)]
    /// Create a new `CooperativeCancellation` config in measure mode with a timeout.
    pub(crate) fn measure_with_timeout(timeout: Duration) -> Self {
        Self {
            enabled: true,
            mode: Mode::Measure,
            timeout: Some(timeout),
        }
    }
}
