use crate::configuration::mode::Mode;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub(crate) type CooperativeCancellation = Mode<Config>;

impl CooperativeCancellation {
    /// Returns the timeout in seconds if cooperative cancellation is enabled with a timeout.
    pub(crate) fn timeout_in_seconds(&self) -> Option<f64> {
        self.inner().and_then(|it| it.timeout_in_seconds())
    }

    /// Create a new `CooperativeCancellation` config in enforcement mode.
    pub(crate) fn enabled() -> Self {
        Self::Enforce(Config::Enabled)
    }

    /// Create a new `CooperativeCancellation` config in enforcement mode with a timeout.
    pub(crate) fn enabled_with_timeout_in_seconds(timeout: f64) -> Self {
        Self::Enforce(Config::EnabledWithTimeoutInSeconds(timeout))
    }
}

/// Controls cooperative cancellation of query planning.
///
/// When enabled, query planning will be cancelled if the client waiting on the query plan closes
/// their connection. Additionally, when enabled with a timeout, the query planning will be
/// cancelled if it takes longer than the specified timeout.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) enum Config {
    /// Enables cooperative cancellation of query planning, but does not set a timeout.
    #[default]
    Enabled,
    /// Enables cooperative cancellation of query planning with a timeout.
    EnabledWithTimeoutInSeconds(f64),
}

impl Config {
    /// Returns the timeout in seconds if cooperative cancellation is enabled with a timeout.
    pub(crate) fn timeout_in_seconds(&self) -> Option<f64> {
        match self {
            Config::EnabledWithTimeoutInSeconds(timeout) => Some(*timeout),
            _ => None,
        }
    }
}
