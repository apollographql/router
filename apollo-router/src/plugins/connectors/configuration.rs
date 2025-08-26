use std::collections::HashMap;
use std::sync::Arc;

use apollo_federation::connectors::CustomConfiguration;
use apollo_federation::connectors::SourceName;
use apollo_federation::connectors::expand::Connectors;
use http::Uri;
use schemars::JsonSchema;
use schemars::schema::InstanceType;
use schemars::schema::SchemaObject;
use serde::Deserialize;
use serde::Serialize;

use super::incompatible::warn_incompatible_plugins;
use crate::Configuration;
use crate::plugins::connectors::plugin::PLUGIN_NAME;
use crate::services::connector_service::ConnectorSourceRef;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConnectorsConfig {
    /// A map of subgraph name to connectors config for that subgraph
    #[serde(default)]
    #[deprecated(note = "use `sources`")]
    pub(crate) subgraphs: HashMap<String, SubgraphConnectorConfiguration>,

    /// Map of subgraph_name.connector_source_name to source configuration
    #[serde(default)]
    pub(crate) sources: HashMap<String, SourceConfiguration>,

    /// Enables connector debugging information on response extensions if the feature is enabled
    #[serde(default)]
    pub(crate) debug_extensions: bool,

    /// The maximum number of requests for a connector source
    #[serde(default)]
    pub(crate) max_requests_per_operation_per_source: Option<usize>,

    /// When enabled, adds an entry to the context for use in coprocessors
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
    #[serde(default)]
    pub(crate) expose_sources_in_context: bool,

    /// Enables Connect spec v0.2 during the preview.
    #[serde(default)]
    #[deprecated(note = "Connect spec v0.2 is now available.")]
    pub(crate) preview_connect_v0_2: Option<bool>,

    /// Feature gate for Connect spec v0.3. Set to `true` to enable the using
    /// the v0.3 spec during the preview phase.
    #[serde(default)]
    pub(crate) preview_connect_v0_3: Option<bool>,
}

// TODO: remove this after deprecation period
/// Configuration for a connector subgraph
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SubgraphConnectorConfiguration {
    /// A map of `@source(name:)` to configuration for that source
    pub(crate) sources: HashMap<SourceName, SourceConfiguration>,

    /// Other values that can be used by connectors via `{$config.<key>}`
    #[serde(rename = "$config")]
    pub(crate) custom: CustomConfiguration,
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

fn uri_schema(_generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
    SchemaObject {
        instance_type: Some(InstanceType::String.into()),
        format: Some("uri".to_owned()),
        extensions: {
            let mut map = schemars::Map::new();
            map.insert("nullable".to_owned(), serde_json::json!(true));
            map
        },
        ..Default::default()
    }
    .into()
}

/// Modifies connectors with values from the configuration
pub(crate) fn apply_config(
    router_config: &Configuration,
    mut connectors: Connectors,
) -> Connectors {
    // Enabling connectors might end up interfering with other router features, so we insert warnings
    // into the logs for any incompatibilities found.
    warn_incompatible_plugins(router_config, &connectors);

    let Some(config) = router_config.apollo_plugins.plugins.get(PLUGIN_NAME) else {
        return connectors;
    };
    let Ok(config) = serde_json::from_value::<ConnectorsConfig>(config.clone()) else {
        return connectors;
    };

    for connector in Arc::make_mut(&mut connectors.by_service_name).values_mut() {
        if let Ok(source_ref) = ConnectorSourceRef::try_from(&mut *connector)
            && let Some(source_config) = config.sources.get(&source_ref.to_string())
        {
            if let Some(uri) = source_config.override_url.as_ref() {
                // Discards potential StringTemplate parsing error as URI should
                // always be a valid template string.
                connector.transport.source_template = uri.to_string().parse().ok();
            }
            if let Some(max_requests) = source_config.max_requests_per_operation {
                connector.max_requests = Some(max_requests);
            }
            connector.config = Some(source_config.custom.clone());
        }

        // TODO: remove this after deprecation period
        #[allow(deprecated)]
        let Some(subgraph_config) = config.subgraphs.get(&connector.id.subgraph_name) else {
            continue;
        };
        if let Some(source_config) = connector
            .id
            .source_name
            .as_ref()
            .and_then(|source_name| subgraph_config.sources.get(source_name))
        {
            if let Some(uri) = source_config.override_url.as_ref() {
                // Discards potential StringTemplate parsing error as
                // URI should always be a valid template string.
                connector.transport.source_template = uri.to_string().parse().ok();
            }
            if let Some(max_requests) = source_config.max_requests_per_operation {
                connector.max_requests = Some(max_requests);
            }
        }
        connector.config = Some(subgraph_config.custom.clone());
    }
    connectors
}
