use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

use crate::configuration::mode::Mode;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(crate) struct CooperativeCancellation {
    enabled: bool,
    // When enabled, this sets whether the router will cancel query planning or
    // merely emit a metric when it would have happened.
    #[serde(default = "Mode::measure_mode")]
    mode: Mode,
    /// The timeout in seconds.
    timeout_in_seconds: Option<f64>,
}

impl Default for CooperativeCancellation {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: Mode::Measure,
            timeout_in_seconds: None,
        }
    }
}

impl CooperativeCancellation {
    /// Returns the timeout in seconds if cooperative cancellation is enabled with a timeout.
    pub(crate) fn timeout_in_seconds(&self) -> Option<f64> {
        self.timeout_in_seconds
    }

    /// Returns the mode of cooperative cancellation.
    pub(crate) fn mode(&self) -> Mode {
        self.mode
    }

    #[cfg(test)]
    /// Create a new `CooperativeCancellation` config in enforcement mode.
    pub(crate) fn enabled() -> Self {
        Self {
            enabled: true,
            mode: Mode::Enforce,
            timeout_in_seconds: None,
        }
    }

    /// Returns true if cooperative cancellation is enabled.
    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled
    }

    #[cfg(test)]
    /// Create a new `CooperativeCancellation` config in enforcement mode with a timeout.
    pub(crate) fn enabled_with_timeout_in_seconds(timeout: f64) -> Self {
        Self {
            enabled: true,
            mode: Mode::Enforce,
            timeout_in_seconds: Some(timeout),
        }
    }
}
