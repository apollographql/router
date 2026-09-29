use std::collections::HashMap;
use std::sync::Arc;

use apollo_federation::connectors::CustomConfiguration;
use apollo_federation::connectors::expand::Connectors;
use http::Uri;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

use super::incompatible::warn_incompatible_plugins;
use crate::Configuration;
use crate::plugins::connectors::plugin::PLUGIN_NAME;
use crate::services::connector_service::ConnectorSourceRef;

/// Configuration for Apollo Connectors.
///
/// https://www.apollographql.com/docs/graphos/routing/configuration/yaml#connectors
#[apollo_configuration::configuration]
#[derive(Serialize)]
pub(crate) struct ConnectorsConfig {
    /// Map of subgraph_name.connector_source_name to source configuration
    #[config(skip_validate)]
    pub(crate) sources: HashMap<String, SourceConfiguration>,

    /// Enables connector debugging information on response extensions if the feature is enabled
    pub(crate) debug_extensions: bool,

    /// The maximum number of requests for a connector source
    pub(crate) max_requests_per_operation_per_source: Option<usize>,

    /// When enabled, adds an entry to the context for use in coprocessors
    ///
    /// ```json
    /// {
    ///   "context": {
    ///     "entries": {
    ///       "apollo_connectors::sources_in_query_plan": [
    ///         { "subgraph_name": "subgraph", "source_name": "source" }
    ///       ]
    ///     }
    ///   }
    /// }
    /// ```
    pub(crate) expose_sources_in_context: bool,

    // The deprecated `preview_connect_*` flags below are no-ops that are still accepted so that
    // existing configurations load. The schema marks them deprecated for editors; nothing reads
    // them, so they don't need Rust's `#[deprecated]`.
    /// Enables Connect spec v0.2 during the preview.
    #[schemars(extend("deprecated" = true))]
    pub(crate) preview_connect_v0_2: Option<bool>,

    /// Feature gate for Connect spec v0.3. Set to `true` to enable the using
    /// the v0.3 spec during the preview phase.
    #[schemars(extend("deprecated" = true))]
    pub(crate) preview_connect_v0_3: Option<bool>,

    /// Feature gate for Connect spec v0.4. Previously required to opt into the
    /// v0.4 spec during its preview phase; now a no-op, since `@link`-ing
    /// connect/v0.4 in a subgraph is itself a sufficient opt-in.
    #[schemars(extend("deprecated" = true))]
    pub(crate) preview_connect_v0_4: Option<bool>,

    /// Feature gate for Connect spec v0.5. Set to `true` to enable using
    /// the v0.5 spec during the preview phase.
    pub(crate) preview_connect_v0_5: Option<bool>,
}

/// Configuration for a `@source` directive
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SourceConfiguration {
    /// Override the `@source(http: {baseURL:})`
    #[serde(default, with = "http_serde::option::uri")]
    #[schemars(schema_with = "uri_schema")]
    pub(crate) override_url: Option<Uri>,

    /// The maximum number of requests for this source
    pub(crate) max_requests_per_operation: Option<usize>,

    /// Other values that can be used by connectors via `{$config.<key>}`
    #[serde(rename = "$config")]
    pub(crate) custom: CustomConfiguration,
}

fn uri_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": ["string", "null"],
        "format": "uri",
    })
}

/// Modifies connectors with values from the configuration
pub(crate) fn apply_config(
    router_config: &Configuration,
    mut connectors: Connectors,
) -> Connectors {
    // Enabling connectors might end up interfering with other router features, so we insert warnings
    // into the logs for any incompatibilities found.
    warn_incompatible_plugins(router_config, &connectors);

    let Some(config) =
        router_config.typed_plugin_config::<ConnectorsConfig>(&format!("apollo.{PLUGIN_NAME}"))
    else {
        return connectors;
    };

    for connector in Arc::make_mut(&mut connectors.by_service_name).values_mut() {
        if let Ok(source_ref) = ConnectorSourceRef::try_from(&mut *connector)
            && let Some(source_config) = config.sources.get(&source_ref.to_string())
        {
            if let Some(uri) = source_config.override_url.as_ref() {
                // Discards potential StringTemplate parsing error as URI should
                // always be a valid template string.
                if let Some(transport) = connector.transport.as_mut() {
                    transport.source_template = uri.to_string().parse().ok();
                }
            }
            if let Some(max_requests) = source_config.max_requests_per_operation {
                connector.max_requests = Some(max_requests);
            }
            connector.config = Some(source_config.custom.clone());
        }
    }
    connectors
}
