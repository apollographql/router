use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use bytesize::ByteSize;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct WasmConfig {
    pub(super) defaults: WasmDefaults,
    pub(super) plugins: Vec<WasmPluginConfig>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct WasmDefaults {
    pub(super) limits: WasmLimits,
    pub(super) failure: WasmFailure,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct WasmPluginConfig {
    pub(super) name: String,
    #[serde(default = "default_enabled")]
    pub(super) enabled: bool,
    pub(super) source: WasmSource,
    #[serde(default = "empty_object")]
    pub(super) configuration: serde_json::Value,
    pub(super) hooks: Vec<WasmHookConfig>,
    #[serde(default)]
    pub(super) limits: WasmLimitsOverride,
    #[serde(default)]
    pub(super) failure: Option<WasmFailure>,
}

fn default_enabled() -> bool {
    true
}

fn empty_object() -> serde_json::Value {
    serde_json::Value::Object(Default::default())
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum WasmSource {
    File {
        path: PathBuf,
        #[serde(default)]
        digest: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct WasmHookConfig {
    pub(super) hook: WasmHook,
    #[serde(default)]
    pub(super) selector: WasmHookSelector,
    #[serde(default)]
    pub(super) permissions: WasmPermissions,
    #[serde(default)]
    pub(super) failure: Option<WasmFailure>,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
pub(super) enum WasmHook {
    #[serde(rename = "supergraph.request")]
    SupergraphRequest,
    #[serde(rename = "subgraph.request")]
    SubgraphRequest,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct WasmHookSelector {
    pub(super) service_names: HashSet<String>,
}

impl WasmHookSelector {
    pub(super) fn matches_service(&self, service_name: &str) -> bool {
        self.service_names.is_empty() || self.service_names.contains(service_name)
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct WasmPermissions {
    pub(super) headers: WasmHeaderPermissions,
    pub(super) context: WasmContextPermissions,
    pub(super) graphql: WasmGraphqlPermissions,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct WasmHeaderPermissions {
    pub(super) read: WasmNameMatcher,
    pub(super) write: WasmNameMatcher,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct WasmContextPermissions {
    pub(super) read: WasmNameMatcher,
    pub(super) write: WasmNameMatcher,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct WasmNameMatcher {
    pub(super) names: HashSet<String>,
}

impl WasmNameMatcher {
    pub(super) fn contains(&self, name: &str) -> bool {
        self.names
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(name))
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct WasmGraphqlPermissions {
    pub(super) request: WasmGraphqlAccess,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum WasmGraphqlAccess {
    #[default]
    None,
    Read,
    ReadWrite,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct WasmLimits {
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[schemars(with = "String")]
    pub(super) execution_timeout: Duration,
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[schemars(with = "String")]
    pub(super) queue_timeout: Duration,
    #[schemars(with = "String")]
    pub(super) max_memory_per_instance: ByteSize,
    pub(super) max_concurrency: usize,
    pub(super) max_queue_size: usize,
    #[schemars(with = "String")]
    pub(super) max_input_size: ByteSize,
    #[schemars(with = "String")]
    pub(super) max_output_size: ByteSize,
}

impl Default for WasmLimits {
    fn default() -> Self {
        Self {
            execution_timeout: Duration::from_millis(10),
            queue_timeout: Duration::from_millis(5),
            max_memory_per_instance: ByteSize::mib(16),
            max_concurrency: 128,
            max_queue_size: 256,
            max_input_size: ByteSize::mib(1),
            max_output_size: ByteSize::mib(1),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct WasmLimitsOverride {
    #[serde(default, deserialize_with = "deserialize_optional_duration")]
    #[schemars(with = "Option<String>")]
    pub(super) execution_timeout: Option<Duration>,
    #[serde(default, deserialize_with = "deserialize_optional_duration")]
    #[schemars(with = "Option<String>")]
    pub(super) queue_timeout: Option<Duration>,
    #[schemars(with = "Option<String>")]
    pub(super) max_memory_per_instance: Option<ByteSize>,
    pub(super) max_concurrency: Option<usize>,
    pub(super) max_queue_size: Option<usize>,
    #[schemars(with = "Option<String>")]
    pub(super) max_input_size: Option<ByteSize>,
    #[schemars(with = "Option<String>")]
    pub(super) max_output_size: Option<ByteSize>,
}

impl WasmLimitsOverride {
    pub(super) fn apply_to(self, mut limits: WasmLimits) -> WasmLimits {
        limits.execution_timeout = self.execution_timeout.unwrap_or(limits.execution_timeout);
        limits.queue_timeout = self.queue_timeout.unwrap_or(limits.queue_timeout);
        limits.max_memory_per_instance = self
            .max_memory_per_instance
            .unwrap_or(limits.max_memory_per_instance);
        limits.max_concurrency = self.max_concurrency.unwrap_or(limits.max_concurrency);
        limits.max_queue_size = self.max_queue_size.unwrap_or(limits.max_queue_size);
        limits.max_input_size = self.max_input_size.unwrap_or(limits.max_input_size);
        limits.max_output_size = self.max_output_size.unwrap_or(limits.max_output_size);
        limits
    }
}

fn deserialize_optional_duration<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    humantime_serde::deserialize(deserializer).map(Some)
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct WasmFailure {
    pub(super) default: WasmFailureMode,
}

impl Default for WasmFailure {
    fn default() -> Self {
        Self {
            default: WasmFailureMode::Closed,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum WasmFailureMode {
    Open,
    #[default]
    Closed,
}
