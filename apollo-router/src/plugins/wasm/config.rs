use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use bytesize::ByteSize;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
/// Configures sandboxed WebAssembly plugins for router request hooks.
pub(super) struct WasmConfig {
    /// Default resource limits and failure behavior for all WebAssembly plugins.
    pub(super) defaults: WasmDefaults,
    /// WebAssembly plugins loaded by the router.
    pub(super) plugins: Vec<WasmPluginConfig>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct WasmDefaults {
    /// Default resource limits applied to WebAssembly plugins.
    pub(super) limits: WasmLimits,
    /// Default behavior when a WebAssembly plugin fails.
    pub(super) failure: WasmFailure,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct WasmPluginConfig {
    /// Unique name used to identify the plugin.
    pub(super) name: String,
    #[serde(default = "default_enabled")]
    /// Enables or disables the plugin.
    pub(super) enabled: bool,
    /// Location and integrity information for the WebAssembly component.
    pub(super) source: WasmSource,
    #[serde(default = "empty_object")]
    /// Plugin-defined configuration passed to the WebAssembly component.
    pub(super) configuration: serde_json::Value,
    /// Router request hooks that invoke the plugin.
    pub(super) hooks: Vec<WasmHookConfig>,
    #[serde(default)]
    /// Resource limit overrides for this plugin.
    pub(super) limits: WasmLimitsOverride,
    #[serde(default)]
    /// Failure behavior override for this plugin.
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
        /// Path to the WebAssembly component file.
        path: PathBuf,
        #[serde(default)]
        /// Optional SHA-256 digest used to verify the component, formatted as `sha256:<hex>`.
        digest: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct WasmHookConfig {
    /// Request stage at which the plugin runs.
    pub(super) hook: WasmHook,
    #[serde(default)]
    /// Services or connectors to which this hook applies.
    pub(super) selector: WasmHookSelector,
    #[serde(default)]
    /// Router data that the plugin may read or modify.
    pub(super) permissions: WasmPermissions,
    #[serde(default)]
    /// Failure behavior override for this hook.
    pub(super) failure: Option<WasmFailure>,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
pub(super) enum WasmHook {
    #[serde(rename = "supergraph.request")]
    Supergraph,
    #[serde(rename = "subgraph.request")]
    Subgraph,
    #[serde(rename = "connector.request")]
    Connector,
}

impl WasmHook {
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::Supergraph => "supergraph.request",
            Self::Subgraph => "subgraph.request",
            Self::Connector => "connector.request",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct WasmHookSelector {
    /// Subgraph or connector service names matched by this hook.
    pub(super) service_names: HashSet<String>,
    /// Connector source names matched by this hook.
    pub(super) source_names: HashSet<String>,
    /// Connector names matched by this hook.
    pub(super) connector_names: HashSet<String>,
}

impl WasmHookSelector {
    pub(super) fn matches_service(&self, service_name: &str) -> bool {
        self.service_names.is_empty() || self.service_names.contains(service_name)
    }

    pub(super) fn matches_connector(
        &self,
        service_name: &str,
        source_name: Option<&str>,
        connector_name: &str,
    ) -> bool {
        self.matches_service(service_name)
            && (self.source_names.is_empty()
                || source_name.is_some_and(|name| self.source_names.contains(name)))
            && (self.connector_names.is_empty() || self.connector_names.contains(connector_name))
    }
}

impl WasmHookConfig {
    pub(super) fn validate(&self, plugin_name: &str) -> Result<(), String> {
        if self.hook == WasmHook::Supergraph
            && (!self.selector.service_names.is_empty()
                || !self.selector.source_names.is_empty()
                || !self.selector.connector_names.is_empty())
        {
            return Err(format!(
                "wasm plugin `{plugin_name}` cannot use selectors for `supergraph.request`"
            ));
        }
        if self.hook == WasmHook::Subgraph
            && (!self.selector.source_names.is_empty() || !self.selector.connector_names.is_empty())
        {
            return Err(format!(
                "wasm plugin `{plugin_name}` cannot use connector selectors for `subgraph.request`"
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct WasmPermissions {
    /// HTTP header access granted to the plugin.
    pub(super) headers: WasmHeaderPermissions,
    /// Router context access granted to the plugin.
    pub(super) context: WasmContextPermissions,
    /// GraphQL request access granted to the plugin.
    pub(super) graphql: WasmGraphqlPermissions,
    /// HTTP transport access granted to the plugin.
    pub(super) transport: WasmTransportPermissions,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct WasmHeaderPermissions {
    /// Header names that the plugin may read.
    pub(super) read: WasmNameMatcher,
    /// Header names that the plugin may write.
    pub(super) write: WasmNameMatcher,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct WasmContextPermissions {
    /// Context keys that the plugin may read.
    pub(super) read: WasmNameMatcher,
    /// Context keys that the plugin may write.
    pub(super) write: WasmNameMatcher,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct WasmNameMatcher {
    /// Explicit names or keys granted to the plugin.
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
    /// Access granted to the GraphQL request payload.
    pub(super) request: WasmGraphqlAccess,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct WasmTransportPermissions {
    /// Access granted to the HTTP method.
    pub(super) method: WasmTransportAccess,
    /// Access granted to the request URI.
    pub(super) uri: WasmTransportAccess,
    /// Access granted to the HTTP body.
    pub(super) body: WasmTransportAccess,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum WasmTransportAccess {
    #[default]
    None,
    Read,
    ReadWrite,
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
    #[schemars(with = "String", default = "schema_default_execution_timeout")]
    /// Maximum execution time for one plugin invocation.
    pub(super) execution_timeout: Duration,
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[schemars(with = "String", default = "schema_default_queue_timeout")]
    /// Maximum time an invocation may wait for an execution slot.
    pub(super) queue_timeout: Duration,
    #[schemars(with = "String")]
    /// Maximum linear memory available to each plugin instance.
    pub(super) max_memory_per_instance: ByteSize,
    /// Maximum number of concurrent invocations.
    pub(super) max_concurrency: usize,
    /// Maximum number of invocations waiting for an execution slot.
    pub(super) max_queue_size: usize,
    #[schemars(with = "String")]
    /// Maximum request payload size passed to the plugin.
    pub(super) max_input_size: ByteSize,
    #[schemars(with = "String")]
    /// Maximum response payload size accepted from the plugin.
    pub(super) max_output_size: ByteSize,
}

const fn schema_default_execution_timeout() -> &'static str {
    "10ms"
}

const fn schema_default_queue_timeout() -> &'static str {
    "5ms"
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
    /// Maximum execution time for one plugin invocation.
    pub(super) execution_timeout: Option<Duration>,
    #[serde(default, deserialize_with = "deserialize_optional_duration")]
    #[schemars(with = "Option<String>")]
    /// Maximum time an invocation may wait for an execution slot.
    pub(super) queue_timeout: Option<Duration>,
    #[schemars(with = "Option<String>")]
    /// Maximum linear memory available to each plugin instance.
    pub(super) max_memory_per_instance: Option<ByteSize>,
    /// Maximum number of concurrent invocations.
    pub(super) max_concurrency: Option<usize>,
    /// Maximum number of invocations waiting for an execution slot.
    pub(super) max_queue_size: Option<usize>,
    #[schemars(with = "Option<String>")]
    /// Maximum request payload size passed to the plugin.
    pub(super) max_input_size: Option<ByteSize>,
    #[schemars(with = "Option<String>")]
    /// Maximum response payload size accepted from the plugin.
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
    /// Whether plugin failures reject the request (`closed`) or allow it to continue (`open`).
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
