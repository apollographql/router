use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde::de::Deserializer;
use serde_json_bytes::ByteString;
use tower::BoxError;
use tower::ServiceExt;

use crate::json_ext::Object;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::subgraph;
use crate::services::SubgraphResponse;

static REDACTED_ERROR_MESSAGE: &str = "Subgraph errors redacted";

register_plugin!("apollo", "include_subgraph_errors", IncludeSubgraphErrors);

/// Configuration for exposing errors that originate from subgraphs
#[derive(Clone, Debug, JsonSchema, Default, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
struct Config {
    /// Configuration for all subgraphs, and handles subgraph errors
    all: ErrorMode,

    /// Override default configuration for specific subgraphs
    subgraphs: HashMap<String, SubgraphConfig>,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
#[serde(untagged)]
enum ErrorMode {
    /// Propagate original error or redact everything
    Included(bool),
    /// Allow specific extension keys with required redact_message
    Allow {
        allow_extensions_keys: Vec<String>,
        redact_message: bool,
    },
    /// Deny specific extension keys with required redact_message
    Deny {
        deny_extensions_keys: Vec<String>,
        redact_message: bool,
    },
}

impl Default for ErrorMode {
    fn default() -> Self {
        ErrorMode::Included(false)
    }
}

impl<'de> Deserialize<'de> for ErrorMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::{self, Visitor};
        use std::fmt;

        struct ErrorModeVisitor;

        impl<'de> Visitor<'de> for ErrorModeVisitor {
            type Value = ErrorMode;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("boolean or object with allow_extensions_keys/deny_extensions_keys")
            }

            fn visit_bool<E>(self, value: bool) -> Result<ErrorMode, E>
            where
                E: de::Error,
            {
                Ok(ErrorMode::Included(value))
            }

            fn visit_map<M>(self, map: M) -> Result<ErrorMode, M::Error>
            where
                M: de::MapAccess<'de>,
            {
                #[derive(Deserialize)]
                struct Helper {
                    allow_extensions_keys: Option<Vec<String>>,
                    deny_extensions_keys: Option<Vec<String>>,
                    redact_message: bool,
                }

                let helper = Helper::deserialize(de::value::MapAccessDeserializer::new(map))?;
                
                match (helper.allow_extensions_keys, helper.deny_extensions_keys) {
                    (Some(_), Some(_)) => {
                        return Err(de::Error::custom(
                            "Global config cannot have both allow_extensions_keys and deny_extensions_keys"
                        ));
                    }
                    (Some(allow), None) => Ok(ErrorMode::Allow {
                        allow_extensions_keys: allow,
                        redact_message: helper.redact_message,
                    }),
                    (None, Some(deny)) => Ok(ErrorMode::Deny {
                        deny_extensions_keys: deny,
                        redact_message: helper.redact_message,
                    }),
                    (None, None) => Ok(ErrorMode::Included(true)),
                }
            }
        }

        deserializer.deserialize_any(ErrorModeVisitor)
    }
}

#[derive(Clone, Debug, JsonSchema, Serialize, Deserialize)]
struct SubgraphConfigCommon {
    #[serde(skip_serializing_if = "Option::is_none")]
    redact_message: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exclude_global_keys: Option<Vec<String>>,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
#[serde(untagged)]
enum SubgraphConfig {
    /// Enable or disable error inclusion for a subgraph
    Included(bool),
    /// Allow specific extension keys for a subgraph
    Allow {
        allow_extensions_keys: Vec<String>,
        #[serde(flatten)]
        common: SubgraphConfigCommon,
    },
    /// Deny specific extension keys for a subgraph
    Deny {
        deny_extensions_keys: Vec<String>,
        #[serde(flatten)]
        common: SubgraphConfigCommon,
    },
    CommonOnly {
        #[serde(flatten)]
        common: SubgraphConfigCommon,
    }
}

// Custom deserializer to handle both boolean and object types
impl<'de> Deserialize<'de> for SubgraphConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{self, Visitor};
        use std::fmt;

        struct SubgraphConfigVisitor;

        impl<'de> Visitor<'de> for SubgraphConfigVisitor {
            type Value = SubgraphConfig;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str(
                    "boolean or object with either allow_extensions_keys or deny_extensions_keys, but not both",
                )
            }

            fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(SubgraphConfig::Included(value))
            }

            fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
            where
                M: de::MapAccess<'de>,
            {
                #[derive(Deserialize)]
                struct FullConfig {
                    allow_extensions_keys: Option<Vec<String>>,
                    deny_extensions_keys: Option<Vec<String>>,
                    redact_message: Option<bool>,
                    exclude_global_keys: Option<Vec<String>>,
                    #[serde(flatten)]
                    extra: HashMap<String, serde_json::Value>,
                }
            
                let config: FullConfig = Deserialize::deserialize(
                    de::value::MapAccessDeserializer::new(map)
                )?;
            
                if !config.extra.is_empty() {
                    return Err(de::Error::custom(format!(
                        "Unknown field(s): {}",
                        config.extra.keys().map(|k| k.as_str()).collect::<Vec<_>>().join(", ")
                    )));
                }
            
                // Ensure error stops deserialization
                match (config.allow_extensions_keys, config.deny_extensions_keys) {
                    (Some(_), Some(_)) => {
                        Err(de::Error::custom(
                            "A subgraph config cannot have both allow_extensions_keys and deny_extensions_keys"
                        ))
                    },
                    (Some(allow), None) => Ok(SubgraphConfig::Allow {
                        allow_extensions_keys: allow,
                        common: SubgraphConfigCommon {
                            redact_message: config.redact_message,
                            exclude_global_keys: config.exclude_global_keys,
                        },
                    }),
                    (None, Some(deny)) => Ok(SubgraphConfig::Deny {
                        deny_extensions_keys: deny,
                        common: SubgraphConfigCommon {
                            redact_message: config.redact_message,
                            exclude_global_keys: config.exclude_global_keys,
                        },
                    }),
                    (None, None) => Ok(SubgraphConfig::CommonOnly {
                        common: SubgraphConfigCommon {
                            redact_message: config.redact_message,
                            exclude_global_keys: config.exclude_global_keys,
                        },
                    }),
                }
            }
        }

        deserializer.deserialize_any(SubgraphConfigVisitor)
    }
}

impl Default for SubgraphConfig {
    fn default() -> Self {
        SubgraphConfig::Included(false)
    }
}

struct IncludeSubgraphErrors {
    config: Config,
}

#[async_trait::async_trait]
impl Plugin for IncludeSubgraphErrors {
    type Config = Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        // Validate global config
        if let ErrorMode::Included(_) = &init.config.all {
            for (name, config) in &init.config.subgraphs {
                if !matches!(config, SubgraphConfig::Included(_)) {
                    return Err(format!(
                        "Subgraph '{}' must use boolean config when global config is boolean or not present",
                        name
                    ).into());
                }
            }
        }
    
        Ok(IncludeSubgraphErrors {
            config: init.config,
        })
    }

    fn subgraph_service(&self, name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
        let subgraph_config = self.config.subgraphs.get(name).cloned();

        let (global_enabled, global_allow, global_deny, should_redact_message) =
            match self.config.all.clone() {
                ErrorMode::Allow {
                    allow_extensions_keys,
                    redact_message,
                } => (true, Some(allow_extensions_keys), None, redact_message),
                ErrorMode::Deny {
                    deny_extensions_keys,
                    redact_message,
                } => (true, None, Some(deny_extensions_keys), redact_message),
                // Set should_redact_message to true when enabled is false
                ErrorMode::Included(enabled) => (enabled, None, None, !enabled),
            };

        // Determine if we should include errors based on subgraph override or global setting
        let include_subgraph_errors = match &subgraph_config {
            Some(SubgraphConfig::Included(enabled)) => *enabled,
            Some(SubgraphConfig::Allow { .. }) => true,
            Some(SubgraphConfig::Deny { .. }) => true,
            Some(SubgraphConfig::CommonOnly { .. }) => true,
            None => global_enabled,
        };

        // Compute effective configuration by merging global and subgraph settings.
        let (effective_allow, effective_deny, effective_redact) =
            if let Some(ref sub_config) = subgraph_config {
                match sub_config {
                    SubgraphConfig::Allow {
                        allow_extensions_keys: sub_allow,
                        common: SubgraphConfigCommon {
                            redact_message: sub_redact,
                            exclude_global_keys,
                        },
                    } => {
                        let redact = sub_redact.unwrap_or(should_redact_message);
                        match &global_allow {
                            Some(global_allow) => {
                                let mut allow_list = global_allow.clone();

                                // Remove any keys that should be overridden
                                if let Some(exclude_keys) = exclude_global_keys {
                                    allow_list.retain(|key| !exclude_keys.contains(key));
                                }

                                // Add subgraph's allow keys
                                allow_list.extend(sub_allow.iter().cloned());
                                allow_list.sort();
                                allow_list.dedup();

                                (Some(allow_list), None, redact)
                            }
                            None => (Some(sub_allow.clone()), None, redact),
                        }
                    }
                    SubgraphConfig::Deny {
                        deny_extensions_keys: sub_deny,
                        common: SubgraphConfigCommon {
                            redact_message: sub_redact,
                            exclude_global_keys,
                        },
                    } => {
                        let redact = sub_redact.unwrap_or(should_redact_message);
                        match &global_deny {
                            Some(global_deny) => {
                                let mut deny_list = global_deny.clone();
                                // Remove excluded keys from global
                                if let Some(exclude_keys) = exclude_global_keys {
                                    deny_list.retain(|key| !exclude_keys.contains(key));
                                }
                                // Now merge sub_deny
                                deny_list.extend(sub_deny.clone());
                                deny_list.sort();
                                deny_list.dedup();
                                (None, Some(deny_list), redact)
                            }
                            None => (None, Some(sub_deny.clone()), redact),
                        }
                    }
                    SubgraphConfig::Included(enabled) => (
                        // Discard global allow/deny when subgraph is bool
                        None,
                        None,
                        if *enabled {
                            false // no redaction when subgraph is true
                        } else {
                            true  // full redaction when subgraph is false
                        },
                    ),
                    SubgraphConfig::CommonOnly {
                        common: SubgraphConfigCommon {
                            redact_message: sub_redact,
                            exclude_global_keys: _,
                        },
                    } => {
                        let redact = sub_redact.unwrap_or(should_redact_message);
                        (None, None, redact)
                    }
                }
            } else {
                match self.config.all.clone() {
                    ErrorMode::Allow {
                        allow_extensions_keys,
                        redact_message,
                    } => (Some(allow_extensions_keys), None, redact_message),
                    ErrorMode::Deny {
                        deny_extensions_keys,
                        redact_message,
                    } => (None, Some(deny_extensions_keys), redact_message),
                    ErrorMode::Included(_) => (None, None, should_redact_message),
                }
            };

        let sub_name_response = name.to_string();
        let sub_name_error = name.to_string();
        service
            .map_response(move |mut response: SubgraphResponse| {
                let errors = &mut response.response.body_mut().errors;
                if !errors.is_empty() {
                    if !include_subgraph_errors {
                        tracing::info!(
                            "redacted subgraph({sub_name_response}) errors - subgraph config"
                        );
                        // Redact based on subgraph config
                        for error in response.response.body_mut().errors.iter_mut() {
                            if effective_redact {
                                error.message = REDACTED_ERROR_MESSAGE.to_string();
                            }
                            // Remove all extensions unless they appear in effective_allow
                            let mut new_extensions = Object::new();
                            if let Some(allow_keys) = &effective_allow {
                                for key in allow_keys {
                                    if let Some(value) = error.extensions.get(key.as_str()) {
                                        new_extensions
                                            .insert(ByteString::from(key.clone()), value.clone());
                                    }
                                }
                            }
                            error.extensions = new_extensions;
                        }
                        return response;
                    }

                    for error in errors.iter_mut() {
                        // Handle message redaction based on effective_redact flag
                        if effective_redact {
                            error.message = REDACTED_ERROR_MESSAGE.to_string();
                        }

                        // Always include service name

                        // Filter extensions based on effective_allow if specified
                        if let Some(allow_keys) = &effective_allow {
                            let mut new_extensions = Object::new();
                            for key in allow_keys {
                                if let Some(value) = error.extensions.get(key.as_str()) {
                                    new_extensions
                                        .insert(ByteString::from(key.clone()), value.clone());
                                }
                            }
                            error.extensions = new_extensions;
                        }

                        // Remove extensions based on effective_deny if specified
                        if let Some(deny_keys) = &effective_deny {
                            for key in deny_keys {
                                error.extensions.remove(key.as_str());
                            }
                        }
                    }
                }

                response
            })
            .map_err(move |error: BoxError| {
                if include_subgraph_errors {
                    error
                } else {
                    // Create a redacted error to replace whatever error we have
                    tracing::info!("redacted subgraph({sub_name_error}) error");
                    let reason = if effective_redact {
                        "redacted".to_string()
                    } else {
                        error.to_string()
                    };
                    Box::new(crate::error::FetchError::SubrequestHttpError {
                        status_code: None,
                        service: "redacted".to_string(),
                        reason,
                    })
                }
            })
            .boxed()
    }
}


#[cfg(test)]
mod test {
    use std::sync::Arc;

    use bytes::Bytes;
    use once_cell::sync::Lazy;
    use serde_json::Value as jValue;
    use serde_json_bytes::ByteString;
    use serde_json_bytes::Value;
    use tower::Service;

    use super::*;
    use crate::json_ext::Object;
    use crate::plugin::test::MockSubgraph;
    use crate::plugin::DynPlugin;
    use crate::query_planner::QueryPlannerService;
    use crate::router_factory::create_plugins;
    use crate::services::layers::persisted_queries::PersistedQueryLayer;
    use crate::services::layers::query_analysis::QueryAnalysisLayer;
    use crate::services::router;
    use crate::services::router::service::RouterCreator;
    use crate::services::HasSchema;
    use crate::services::PluggableSupergraphServiceBuilder;
    use crate::services::SupergraphRequest;
    use crate::spec::Schema;
    use crate::Configuration;

    static UNREDACTED_PRODUCT_RESPONSE: Lazy<Bytes> = Lazy::new(|| {
        Bytes::from_static(r#"{"data":{"topProducts":null},"errors":[{"message":"couldn't find mock for query {\"query\":\"query($first: Int) { topProducts(first: $first) { __typename upc } }\",\"variables\":{\"first\":2}}","path":[],"extensions":{"test":"value","code":"FETCH_ERROR","service":"products"}}]}"#.as_bytes())
    });

    static REDACTED_PRODUCT_RESPONSE: Lazy<Bytes> = Lazy::new(|| {
        Bytes::from_static(
            r#"{"data":{"topProducts":null},"errors":[{"message":"Subgraph errors redacted","path":[]}]}"#
                .as_bytes(),
        )
    });

    static REDACTED_ACCOUNT_RESPONSE: Lazy<Bytes> = Lazy::new(|| {
        Bytes::from_static(
            r#"{"data":null,"errors":[{"message":"Subgraph errors redacted","path":[]}]}"#
                .as_bytes(),
        )
    });

    static EXPECTED_RESPONSE: Lazy<Bytes> = Lazy::new(|| {
        Bytes::from_static(r#"{"data":{"topProducts":[{"upc":"1","name":"Table","reviews":[{"id":"1","product":{"name":"Table"},"author":{"id":"1","name":"Ada Lovelace"}},{"id":"4","product":{"name":"Table"},"author":{"id":"2","name":"Alan Turing"}}]},{"upc":"2","name":"Couch","reviews":[{"id":"2","product":{"name":"Couch"},"author":{"id":"1","name":"Ada Lovelace"}}]}]}}"#.as_bytes())
    });

    static VALID_QUERY: &str = r#"query TopProducts($first: Int) { topProducts(first: $first) { upc name reviews { id product { name } author { id name } } } }"#;

    static ERROR_PRODUCT_QUERY: &str = r#"query ErrorTopProducts($first: Int) { topProducts(first: $first) { upc reviews { id product { name } author { id name } } } }"#;

    static ERROR_ACCOUNT_QUERY: &str = r#"query Query { me { name }}"#;

    async fn execute_router_test(
        query: &str,
        body: &Bytes,
        mut router_service: router::BoxService,
    ) {
        let request = SupergraphRequest::fake_builder()
            .query(query.to_string())
            .variable("first", 2usize)
            .build()
            .expect("expecting valid request")
            .try_into()
            .unwrap();

        let response = router_service
            .ready()
            .await
            .unwrap()
            .call(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(*body, response);
    }

    async fn build_mock_router(plugin: Box<dyn DynPlugin>) -> router::BoxService {
        let mut extensions = Object::new();
        extensions.insert("test", Value::String(ByteString::from("value")));

        let account_mocks = vec![
            (
                r#"{"query":"query TopProducts__accounts__3($representations:[_Any!]!){_entities(representations:$representations){...on User{name}}}","operationName":"TopProducts__accounts__3","variables":{"representations":[{"__typename":"User","id":"1"},{"__typename":"User","id":"2"}]}}"#,
                r#"{"data":{"_entities":[{"name":"Ada Lovelace"},{"name":"Alan Turing"}]}}"#
            )
        ].into_iter().map(|(query, response)| (serde_json::from_str(query).unwrap(), serde_json::from_str(response).unwrap())).collect();
        let account_service = MockSubgraph::new(account_mocks);

        let review_mocks = vec![
            (
                r#"{"query":"query TopProducts__reviews__1($representations:[_Any!]!){_entities(representations:$representations){...on Product{reviews{id product{__typename upc}author{__typename id}}}}}","operationName":"TopProducts__reviews__1","variables":{"representations":[{"__typename":"Product","upc":"1"},{"__typename":"Product","upc":"2"}]}}"#,
                r#"{"data":{"_entities":[{"reviews":[{"id":"1","product":{"__typename":"Product","upc":"1"},"author":{"__typename":"User","id":"1"}},{"id":"4","product":{"__typename":"Product","upc":"1"},"author":{"__typename":"User","id":"2"}}]},{"reviews":[{"id":"2","product":{"__typename":"Product","upc":"2"},"author":{"__typename":"User","id":"1"}}]}]}}"#
            )
            ].into_iter().map(|(query, response)| (serde_json::from_str(query).unwrap(), serde_json::from_str(response).unwrap())).collect();
        let review_service = MockSubgraph::new(review_mocks);

        let product_mocks = vec![
            (
                r#"{"query":"query TopProducts__products__0($first:Int){topProducts(first:$first){__typename upc name}}","operationName":"TopProducts__products__0","variables":{"first":2}}"#,
                r#"{"data":{"topProducts":[{"__typename":"Product","upc":"1","name":"Table"},{"__typename":"Product","upc":"2","name":"Couch"}]}}"#
            ),
            (
                r#"{"query":"query TopProducts__products__2($representations:[_Any!]!){_entities(representations:$representations){...on Product{name}}}","operationName":"TopProducts__products__2","variables":{"representations":[{"__typename":"Product","upc":"1"},{"__typename":"Product","upc":"2"}]}}"#,
                r#"{"data":{"_entities":[{"name":"Table"},{"name":"Couch"}]}}"#
            )
            ].into_iter().map(|(query, response)| (serde_json::from_str(query).unwrap(), serde_json::from_str(response).unwrap())).collect();

        let product_service = MockSubgraph::new(product_mocks).with_extensions(extensions);

        let mut configuration = Configuration::default();
        // TODO(@goto-bus-stop): need to update the mocks and remove this, #6013
        configuration.supergraph.generate_query_fragments = false;
        let configuration = Arc::new(configuration);

        let schema =
            include_str!("../../../apollo-router-benchmarks/benches/fixtures/supergraph.graphql");
        let schema = Schema::parse(schema, &configuration).unwrap();

        let planner = QueryPlannerService::new(schema.into(), Arc::clone(&configuration))
            .await
            .unwrap();
        let schema = planner.schema();
        let subgraph_schemas = Arc::new(
            planner
                .subgraph_schemas()
                .iter()
                .map(|(k, v)| (k.clone(), v.schema.clone()))
                .collect(),
        );

        let builder = PluggableSupergraphServiceBuilder::new(planner);

        let mut plugins = create_plugins(&configuration, &schema, subgraph_schemas, None, None)
            .await
            .unwrap();

        plugins.insert("apollo.include_subgraph_errors".to_string(), plugin);

        let builder = builder
            .with_plugins(Arc::new(plugins))
            .with_subgraph_service("accounts", account_service.clone())
            .with_subgraph_service("reviews", review_service.clone())
            .with_subgraph_service("products", product_service.clone());

        let supergraph_creator = builder.build().await.expect("should build");

        RouterCreator::new(
            QueryAnalysisLayer::new(supergraph_creator.schema(), Arc::clone(&configuration)).await,
            Arc::new(PersistedQueryLayer::new(&configuration).await.unwrap()),
            Arc::new(supergraph_creator),
            configuration,
        )
        .await
        .unwrap()
        .make()
        .boxed()
    }

    async fn get_redacting_plugin(config: &jValue) -> Result<Box<dyn DynPlugin>, BoxError> {
        // Build a redacting plugin
        crate::plugin::plugins()
            .find(|factory| factory.name == "apollo.include_subgraph_errors")
            .expect("Plugin not found")
            .create_instance_without_schema(config)
            .await
    }

    #[tokio::test]
    async fn it_returns_valid_response() {
        // Build a redacting plugin
        let plugin = get_redacting_plugin(&serde_json::json!({ "all": false })).await.unwrap();
        let router = build_mock_router(plugin).await;
        execute_router_test(VALID_QUERY, &EXPECTED_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_redacts_all_subgraphs_explicit_redact() {
        // Build a redacting plugin
        let plugin = get_redacting_plugin(&serde_json::json!({ "all": false })).await.unwrap();
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_PRODUCT_QUERY, &REDACTED_PRODUCT_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_redacts_all_subgraphs_implicit_redact() {
        // Build a redacting plugin
        let plugin = get_redacting_plugin(&serde_json::json!({})).await.unwrap();
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_PRODUCT_QUERY, &REDACTED_PRODUCT_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_does_not_redact_all_subgraphs_explicit_allow() {
        // Build a redacting plugin
        let plugin = get_redacting_plugin(&serde_json::json!({ "all": true })).await.unwrap();
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_PRODUCT_QUERY, &UNREDACTED_PRODUCT_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_does_not_redact_all_implicit_redact_product_explict_allow_for_product_query() {
        // Build a redacting plugin
        let plugin =
            get_redacting_plugin(&serde_json::json!({ "subgraphs": {"products": true }})).await.unwrap();
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_PRODUCT_QUERY, &UNREDACTED_PRODUCT_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_does_redact_all_implicit_redact_product_explict_allow_for_review_query() {
        // Build a redacting plugin
        let plugin =
            get_redacting_plugin(&serde_json::json!({ "subgraphs": {"reviews": true }})).await.unwrap();
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_PRODUCT_QUERY, &REDACTED_PRODUCT_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_does_not_redact_all_explicit_allow_review_explict_redact_for_product_query() {
        // Build a redacting plugin
        let plugin = get_redacting_plugin(
            &serde_json::json!({ "all": true, "subgraphs": {"reviews": false }}),
        )
        .await.unwrap();
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_PRODUCT_QUERY, &UNREDACTED_PRODUCT_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_does_redact_all_explicit_allow_product_explict_redact_for_product_query() {
        // Build a redacting plugin
        let plugin = get_redacting_plugin(
            &serde_json::json!({ "all": true, "subgraphs": {"products": false }}),
        )
        .await.unwrap();
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_PRODUCT_QUERY, &REDACTED_PRODUCT_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_does_not_redact_all_explicit_allow_account_explict_redact_for_product_query() {
        // Build a redacting plugin
        let plugin = get_redacting_plugin(
            &serde_json::json!({ "all": true, "subgraphs": {"accounts": false }}),
        )
        .await.unwrap();
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_PRODUCT_QUERY, &UNREDACTED_PRODUCT_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_does_redact_all_explicit_allow_account_explict_redact_for_account_query() {
        // Build a redacting plugin
        let plugin = get_redacting_plugin(
            &serde_json::json!({ "all": true, "subgraphs": {"accounts": false }}),
        )
        .await.unwrap();
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_ACCOUNT_QUERY, &REDACTED_ACCOUNT_RESPONSE, router).await;
    }

    // new test cases for allow and deny list config
    #[tokio::test]
    async fn it_rejects_object_config_when_global_is_boolean() {
        let result = get_redacting_plugin(&serde_json::json!({
            "all": true,
            "subgraphs": {
                "products": {
                    "deny_extensions_keys": ["code"]
                }
            }
        })).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn it_requires_redact_message_when_global_is_object() {
        let result = get_redacting_plugin(&serde_json::json!({
            "all": {
                "deny_extensions_keys": ["code"]
            }
        })).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn it_rejects_global_with_both_allow_and_deny() {
        let result = get_redacting_plugin(&serde_json::json!({
            "all": {
                "redact_message": true,
                "allow_extensions_keys": ["code"],
                "deny_extensions_keys": ["reason"]
            }
        })).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn it_rejects_subgraph_with_both_allow_and_deny() {
        let result = get_redacting_plugin(&serde_json::json!({
            "all": {
                "redact_message": true,
                "deny_extensions_keys": ["code"]
            },
            "subgraphs": {
                "products": {
                    "allow_extensions_keys": ["code"],
                    "deny_extensions_keys": ["service"]
                }
            }
        })).await;
        assert!(result.is_err());
    }

    // test case that needs responses

    static PRODUCT_RESPONSE_WITH_FILTERED_EXTENSIONS: Lazy<Bytes> = Lazy::new(|| {
        Bytes::from_static(r#"{"data":{"topProducts":null},"errors":[{"message":"couldn't find mock","path":[],"extensions":{"code":"ERROR"}}]}"#.as_bytes())
    });

    #[tokio::test]
    async fn it_filters_extensions_based_on_allow_list() {
        let plugin = get_redacting_plugin(&serde_json::json!({
            "all": {
                "redact_message": false,
                "allow_extensions_keys": ["code"]
            }
        })).await.unwrap();
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_PRODUCT_QUERY, &PRODUCT_RESPONSE_WITH_FILTERED_EXTENSIONS, router).await;
    }

    #[tokio::test]
    async fn it_allows_any_config_type_when_global_is_object() {
        let plugin = get_redacting_plugin(&serde_json::json!({
            "all": {
                "redact_message": true,
                "deny_extensions_keys": ["code"]
            },
            "subgraphs": {
                "products": {
                    "allow_extensions_keys": ["code"]  // Opposite list type is allowed
                },
                "reviews": {
                    "deny_extensions_keys": ["reason"]  // Same list type is allowed
                },
                "inventory": {
                    "exclude_global_keys": ["code"]  // CommonOnly is allowed
                },
                "accounts": true  // Boolean is allowed
            }
        })).await;
        assert!(plugin.is_ok());
    }
    
    #[tokio::test]
    async fn a_subgraph_allows_explicit_deny_or_allow_list_when_global_is_object() {
        let plugin = get_redacting_plugin(&serde_json::json!({
            "all": {
                "redact_message": true,
                "deny_extensions_keys": ["code", "reason"]
            },
            "subgraphs": {
                "products": {
                    "redact_message": true,
                    "allow_extensions_keys": ["code"],
                    "exclude_global_keys": ["reason"],
                },
                "reviews": {
                    "redact_message": false,
                    "deny_extensions_keys": ["reason"],
                    "exclude_global_keys": ["code"],
                }
            }
        })).await;
        assert!(plugin.is_ok());
    }
    
    #[tokio::test]
    async fn it_allows_common_only_config_when_global_is_object() {
        let plugin = get_redacting_plugin(&serde_json::json!({
            "all": {
                "redact_message": true,
                "deny_extensions_keys": ["code", "reason"]
            },
            "subgraphs": {
                "products": {
                    "redact_message": true,
                },
                "reviews": {
                    "redact_message": true,
                    "exclude_global_keys": ["code"]
                }
            }
        })).await;
        assert!(plugin.is_ok());
    }

    #[tokio::test]
    async fn subgraph_with_boolean_overrides_global_object() {
        let plugin = get_redacting_plugin(&serde_json::json!({
            "all": {
                "redact_message": true,
                "deny_extensions_keys": ["code", "service"]
            },
            "subgraphs": {
                "products": true
            }
        })).await.unwrap();
        let router = build_mock_router(plugin).await;
        execute_router_test(ERROR_PRODUCT_QUERY, &UNREDACTED_PRODUCT_RESPONSE, router).await;
    }
    
    #[tokio::test]
    async fn it_overrides_global_list_with_subgraph_opposite_list_type() {
        let plugin = get_redacting_plugin(&serde_json::json!({
            "all": {
                "redact_message": true,
                "deny_extensions_keys": ["code", "service"]
            },
            "subgraphs": {
                "products": {
                    "allow_extensions_keys": ["code"]
                }
            }
        })).await.unwrap();
        let router = build_mock_router(plugin).await;
        // Would need to create a specific response that shows only code extension passed through
        execute_router_test(ERROR_PRODUCT_QUERY, &PRODUCT_RESPONSE_WITH_FILTERED_EXTENSIONS, router).await;
    }

}
