use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

// Don't add a default here. Instead, Default should be implemented for
// individual cases of Mode<T>.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Mode {
    Measure,
    Enforce,
}

impl Mode {
    /// Returns true if this config is in measure mode.
    pub(crate) fn is_measure_mode(&self) -> bool {
        matches!(self, Mode::Measure)
    }

    /// Returns true if this config is in enforce mode.
    pub(crate) fn is_enforce_mode(&self) -> bool {
        matches!(self, Mode::Enforce)
    }
}
