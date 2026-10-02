//! A plugin configuration that is a bare `true` or `false`.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

/// The configuration of a plugin whose section is `true` or `false`, for example
/// `forbid_mutations: true`. Its schema is `type: boolean`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(transparent)]
#[schemars(
    inline,
    description = "Set to `true` to enable, or `false` to disable."
)]
pub struct Enabled(pub bool);

// `#[configuration]` does not support tuple structs yet, a named struct would change the YAML, and
// the orphan rule prevents implementing the traits on `bool` itself, so this implements them by
// hand. A bare boolean has no validation rules.
impl apollo_configuration::Validate for Enabled {}
impl apollo_configuration::Configuration for Enabled {}
