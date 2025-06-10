use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) enum Mode<T> {
    Measure(T),
    Enforce(T),
    #[default]
    Disabled,
}

impl<T> Mode<T> {
    /// Returns true if this config is enabled.
    pub(crate) fn is_enabled(&self) -> bool {
        matches!(self, Mode::Measure(_) | Mode::Enforce(_))
    }

    /// Returns true if this config is in measure mode.
    pub(crate) fn is_measure_mode(&self) -> bool {
        matches!(self, Mode::Measure(_))
    }

    /// Returns true if this config is in enforce mode.
    pub(crate) fn is_enforce_mode(&self) -> bool {
        matches!(self, Mode::Enforce(_))
    }

    /// Returns the inner configuration, if it's enabled.
    pub(crate) fn inner(&self) -> Option<&T> {
        match self {
            Mode::Measure(config) | Mode::Enforce(config) => Some(config),
            Mode::Disabled => None,
        }
    }
}
