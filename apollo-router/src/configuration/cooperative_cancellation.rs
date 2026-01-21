use std::time::Duration;

use bytesize::ByteSize;
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
    /// Enable memory limit for query planning.
    #[schemars(with = "Option<String>", default)]
    memory_limit: Option<ByteSize>,
}

impl Default for CooperativeCancellation {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: Mode::Measure,
            timeout: None,
            memory_limit: None,
        }
    }
}

impl CooperativeCancellation {
    /// Returns the timeout, if configured.
    pub(crate) fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    /// Returns the memory limit, if configured.
    pub(crate) fn memory_limit(&self) -> Option<ByteSize> {
        self.memory_limit
    }

    #[cfg(test)]
    /// Create a new `CooperativeCancellation` config in enforcement mode.
    pub(crate) fn enabled() -> Self {
        Self {
            enabled: true,
            mode: Mode::Enforce,
            timeout: None,
            memory_limit: None,
        }
    }

    /// Returns true if cooperative cancellation is enabled.
    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn mode(&self) -> Mode {
        self.mode
    }

    #[cfg(test)]
    /// Create a new `CooperativeCancellation` config in enforcement mode with a timeout.
    pub(crate) fn enabled_with_timeout(timeout: Duration) -> Self {
        Self {
            enabled: true,
            mode: Mode::Enforce,
            timeout: Some(timeout),
            memory_limit: None,
        }
    }

    #[cfg(test)]
    /// Create a new `CooperativeCancellation` config in measure mode with a timeout.
    pub(crate) fn measure_with_timeout(timeout: Duration) -> Self {
        Self {
            enabled: true,
            mode: Mode::Measure,
            timeout: Some(timeout),
            memory_limit: None,
        }
    }

    #[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix, test))]
    /// Create a new `CooperativeCancellation` config in enforce mode with a memory limit.
    pub(crate) fn enforce_with_memory_limit(memory_limit: ByteSize) -> Self {
        Self {
            enabled: true,
            mode: Mode::Enforce,
            timeout: None,
            memory_limit: Some(memory_limit),
        }
    }

    /// Create a new `CooperativeCancellation` config in measure mode with a memory limit.
    #[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix, test))]
    pub(crate) fn measure_with_memory_limit(memory_limit: ByteSize) -> Self {
        Self {
            enabled: true,
            mode: Mode::Measure,
            timeout: None,
            memory_limit: Some(memory_limit),
        }
    }

    #[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix, test))]
    /// Create a new `CooperativeCancellation` config in enforcement mode with both timeout and memory limit.
    pub(crate) fn enforce_with_timeout_and_memory_limit(
        timeout: Duration,
        memory_limit: ByteSize,
    ) -> Self {
        Self {
            enabled: true,
            mode: Mode::Enforce,
            timeout: Some(timeout),
            memory_limit: Some(memory_limit),
        }
    }

    #[cfg(all(feature = "global-allocator", not(feature = "dhat-heap"), unix, test))]
    /// Create a new `CooperativeCancellation` config in measure mode with both timeout and memory limit.
    pub(crate) fn measure_with_timeout_and_memory_limit(
        timeout: Duration,
        memory_limit: ByteSize,
    ) -> Self {
        Self {
            enabled: true,
            mode: Mode::Measure,
            timeout: Some(timeout),
            memory_limit: Some(memory_limit),
        }
    }
}
