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

// Don't add a default here. Instead, Default should be implemented for
// individual cases of WarnOrEnforceMode<T>.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WarnOrEnforceMode {
    Warn,
    Enforce,
}
