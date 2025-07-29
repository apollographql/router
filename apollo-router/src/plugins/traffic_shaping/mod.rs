//! Traffic shaping plugin
//!
//! Currently includes:
//! * Query deduplication
//! * Timeout
//! * Compression
//! * Rate limiting
//!
mod deduplication;

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::time::Duration;

use apollo_federation::connectors::runtime::errors::Error;
use apollo_federation::connectors::runtime::http_json_transport::TransportRequest;
use http::HeaderValue;
use http::StatusCode;
use http::header::CONTENT_ENCODING;
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower::limit::ConcurrencyLimitLayer;
use tower::limit::RateLimitLayer;
use tower::load_shed::error::Overloaded;
use tower::timeout::TimeoutLayer;
use tower::timeout::error::Elapsed;

use self::deduplication::QueryDeduplicationLayer;
use crate::configuration::shared::DnsResolutionStrategy;
use crate::graphql;
use crate::layers::ServiceBuilderExt;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::services::RouterResponse;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;
use crate::services::connector;
use crate::services::connector::request_service::Request;
use crate::services::connector::request_service::Response;
use crate::services::http::service::Compression;
use crate::services::router;
use crate::services::subgraph;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const APOLLO_TRAFFIC_SHAPING: &str = "apollo.traffic_shaping";

trait Merge {
    fn merge(&self, fallback: Option<&Self>) -> Self;
}

/// Traffic shaping options
#[derive(PartialEq, Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Shaping {
    /// Enable query deduplication
    deduplicate_query: Option<bool>,
    /// Enable compression for subgraphs (available compressions are deflate, br, gzip)
    compression: Option<Compression>,
    /// Enable global rate limiting
    global_rate_limit: Option<RateLimitConf>,
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "String", default)]
    /// Enable timeout for incoming requests
    timeout: Option<Duration>,
    /// Enable HTTP2 for subgraphs
    experimental_http2: Option<Http2Config>,
    /// DNS resolution strategy for subgraphs
    dns_resolution_strategy: Option<DnsResolutionStrategy>,
}

#[derive(PartialEq, Default, Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Http2Config {
    #[default]
    /// Enable HTTP2 for subgraphs
    Enable,
    /// Disable HTTP2 for subgraphs
    Disable,
    /// Only HTTP2 is active
    Http2Only,
}

impl Merge for Shaping {
    fn merge(&self, fallback: Option<&Self>) -> Self {
        match fallback {
            None => self.clone(),
            Some(fallback) => Shaping {
                deduplicate_query: self.deduplicate_query.or(fallback.deduplicate_query),
                compression: self.compression.or(fallback.compression),
                timeout: self.timeout.or(fallback.timeout),
                global_rate_limit: self
                    .global_rate_limit
                    .as_ref()
                    .or(fallback.global_rate_limit.as_ref())
                    .cloned(),
                experimental_http2: self
                    .experimental_http2
                    .as_ref()
                    .or(fallback.experimental_http2.as_ref())
                    .cloned(),
                dns_resolution_strategy: self
                    .dns_resolution_strategy
                    .as_ref()
                    .or(fallback.dns_resolution_strategy.as_ref())
                    .cloned(),
            },
        }
    }
}

// this is a wrapper struct to add subgraph specific options over Shaping
#[derive(PartialEq, Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SubgraphShaping {
    #[serde(flatten)]
    shaping: Shaping,
}

impl Merge for SubgraphShaping {
    fn merge(&self, fallback: Option<&Self>) -> Self {
        match fallback {
            None => self.clone(),
            Some(fallback) => SubgraphShaping {
                shaping: self.shaping.merge(Some(&fallback.shaping)),
            },
        }
    }
}

#[derive(PartialEq, Debug, Clone, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields, default)]
struct ConnectorsShapingConfig {
    /// Applied on all connectors
    all: Option<ConnectorShaping>,
    /// Applied on specific connector sources
    sources: HashMap<String, ConnectorShaping>,
}

#[derive(PartialEq, Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ConnectorShaping {
    /// Enable compression for connectors (available compressions are deflate, br, gzip)
    compression: Option<Compression>,
    /// Enable global rate limiting
    global_rate_limit: Option<RateLimitConf>,
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "String", default)]
    /// Enable timeout for connectors requests
    timeout: Option<Duration>,
    /// Enable HTTP2 for connectors
    experimental_http2: Option<Http2Config>,
    /// DNS resolution strategy for connectors
    dns_resolution_strategy: Option<DnsResolutionStrategy>,
}

impl Merge for ConnectorShaping {
    fn merge(&self, fallback: Option<&Self>) -> Self {
        match fallback {
            None => self.clone(),
            Some(fallback) => ConnectorShaping {
                compression: self.compression.or(fallback.compression),
                timeout: self.timeout.or(fallback.timeout),
                global_rate_limit: self
                    .global_rate_limit
                    .as_ref()
                    .or(fallback.global_rate_limit.as_ref())
                    .cloned(),
                experimental_http2: self
                    .experimental_http2
                    .as_ref()
                    .or(fallback.experimental_http2.as_ref())
                    .cloned(),
                dns_resolution_strategy: self
                    .dns_resolution_strategy
                    .as_ref()
                    .or(fallback.dns_resolution_strategy.as_ref())
                    .cloned(),
            },
        }
    }
}

#[derive(PartialEq, Debug, Clone, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
struct RouterShaping {
    /// The global concurrency limit
    concurrency_limit: Option<usize>,

    /// Enable global rate limiting
    global_rate_limit: Option<RateLimitConf>,
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "String", default)]
    /// Enable timeout for incoming requests
    timeout: Option<Duration>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
// FIXME: This struct is pub(crate) because we need its configuration in the query planner service.
// Remove this once the configuration yml changes.
/// Configuration for the experimental traffic shaping plugin
pub(crate) struct Config {
    /// Applied at the router level
    router: Option<RouterShaping>,
    /// Applied on all subgraphs
    all: Option<SubgraphShaping>,
    /// Applied on specific subgraphs
    subgraphs: HashMap<String, SubgraphShaping>,
    /// Applied on specific subgraphs
    connector: ConnectorsShapingConfig,

    /// DEPRECATED, now always enabled: Enable variable deduplication optimization when sending requests to subgraphs (https://github.com/apollographql/router/issues/87)
    deduplicate_variables: Option<bool>,
}

#[derive(PartialEq, Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RateLimitConf {
    /// Number of requests allowed
    capacity: NonZeroU64,
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[schemars(with = "String")]
    /// Per interval
    interval: Duration,
}

impl Merge for RateLimitConf {
    fn merge(&self, fallback: Option<&Self>) -> Self {
        match fallback {
            None => self.clone(),
            Some(fallback) => Self {
                capacity: fallback.capacity,
                interval: fallback.interval,
            },
        }
    }
}

// FIXME: This struct is pub(crate) because we need its configuration in the query planner service.
// Remove this once the configuration yml changes.
pub(crate) struct TrafficShaping {
    config: Config,
    rate_limit_subgraphs: Mutex<HashMap<String, RateLimitLayer>>,
    rate_limit_sources: Mutex<HashMap<String, RateLimitLayer>>,
}

#[async_trait::async_trait]
impl PluginPrivate for TrafficShaping {
    type Config = Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(Self {
            config: init.config,
            rate_limit_subgraphs: Mutex::new(HashMap::new()),
            rate_limit_sources: Mutex::new(HashMap::new()),
        })
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        ServiceBuilder::new()
            .map_future_with_request_data(
                |req: &router::Request| req.context.clone(),
                move |ctx, future| {
                    async {
                        let response: Result<RouterResponse, BoxError> = future.await;
                        match response {
                            Ok(ok) => Ok(ok),
                            Err(err) if err.is::<Elapsed>() => {
                                // TODO add metrics
                                let error = graphql::Error::builder()
                                    .message("Your request has been timed out")
                                    .extension_code("GATEWAY_TIMEOUT")
                                    .build();
                                Ok(RouterResponse::error_builder()
                                    .status_code(StatusCode::GATEWAY_TIMEOUT)
                                    .error(error)
                                    .context(ctx)
                                    .build()
                                    .expect("should build overloaded response"))
                            }
                            Err(err) => Err(err),
                        }
                    }
                },
            )
            .load_shed()
            .layer(TimeoutLayer::new(
                self.config
                    .router
                    .as_ref()
                    .and_then(|r| r.timeout)
                    .unwrap_or(DEFAULT_TIMEOUT),
            ))
            .map_future_with_request_data(
                |req: &router::Request| req.context.clone(),
                move |ctx, future| {
                    async {
                        let response: Result<RouterResponse, BoxError> = future.await;
                        match response {
                            Ok(ok) => Ok(ok),
                            Err(err) if err.is::<Overloaded>() => {
                                // TODO add metrics
                                let error = graphql::Error::builder()
                                    .message("Your request has been concurrency limited")
                                    .extension_code("REQUEST_CONCURRENCY_LIMITED")
                                    .build();
                                Ok(RouterResponse::error_builder()
                                    .status_code(StatusCode::SERVICE_UNAVAILABLE)
                                    .error(error)
                                    .context(ctx)
                                    .build()
                                    .expect("should build overloaded response"))
                            }
                            Err(err) => Err(err),
                        }
                    }
                },
            )
            .load_shed()
            .option_layer(self.config.router.as_ref().and_then(|router| {
                router
                    .concurrency_limit
                    .as_ref()
                    .map(|limit| ConcurrencyLimitLayer::new(*limit))
            }))
            .map_future_with_request_data(
                |req: &router::Request| req.context.clone(),
                move |ctx, future| {
                    async {
                        let response: Result<RouterResponse, BoxError> = future.await;
                        match response {
                            Ok(ok) => Ok(ok),
                            Err(err) if err.is::<Overloaded>() => {
                                // TODO add metrics
                                let error = graphql::Error::builder()
                                    .message("Your request has been rate limited")
                                    .extension_code("REQUEST_RATE_LIMITED")
                                    .build();
                                Ok(RouterResponse::error_builder()
                                    .status_code(StatusCode::SERVICE_UNAVAILABLE)
                                    .error(error)
                                    .context(ctx)
                                    .build()
                                    .expect("should build overloaded response"))
                            }
                            Err(err) => Err(err),
                        }
                    }
                },
            )
            .load_shed()
            .option_layer(self.config.router.as_ref().and_then(|router| {
                router
                    .global_rate_limit
                    .as_ref()
                    .map(|limit| RateLimitLayer::new(limit.capacity.into(), limit.interval))
            }))
            .service(service)
            .boxed()
    }

    fn subgraph_service(&self, name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
        // Either we have the subgraph config and we merge it with the all config, or we just have the all config or we have nothing.
        let all_config = self.config.all.as_ref();
        let subgraph_config = self.config.subgraphs.get(name);
        let final_config = Self::merge_config(all_config, subgraph_config);

        if let Some(config) = final_config {
            let rate_limit = config
                .shaping
                .global_rate_limit
                .as_ref()
                .map(|rate_limit_conf| {
                    self.rate_limit_subgraphs
                        .lock()
                        .entry(name.to_string())
                        .or_insert_with(|| {
                            RateLimitLayer::new(
                                rate_limit_conf.capacity.into(),
                                rate_limit_conf.interval,
                            )
                        })
                        .clone()
                });

            ServiceBuilder::new()
                .map_future_with_request_data(
                    |req: &subgraph::Request| (req.context.clone(), req.subgraph_name.clone()),
                    move |(ctx, subgraph_name), future| {
                        async {
                            let response: Result<SubgraphResponse, BoxError> = future.await;
                            match response {
                                Ok(ok) => Ok(ok),
                                Err(err) if err.is::<Elapsed>() => {
                                    // TODO add metrics
                                    let error = graphql::Error::builder()
                                        .message("Your request has been timed out")
                                        .extension_code("GATEWAY_TIMEOUT")
                                        .build();
                                    Ok(SubgraphResponse::error_builder()
                                        .status_code(StatusCode::GATEWAY_TIMEOUT)
                                        .subgraph_name(subgraph_name)
                                        .error(error)
                                        .context(ctx)
                                        .build())
                                }
                                Err(err) if err.is::<Overloaded>() => {
                                    // TODO add metrics
                                    let error = graphql::Error::builder()
                                        .message("Your request has been rate limited")
                                        .extension_code("REQUEST_RATE_LIMITED")
                                        .build();
                                    Ok(SubgraphResponse::error_builder()
                                        .status_code(StatusCode::SERVICE_UNAVAILABLE)
                                        .subgraph_name(subgraph_name)
                                        .error(error)
                                        .context(ctx)
                                        .build())
                                }
                                Err(err) => Err(err),
                            }
                        }
                    },
                )
                .load_shed()
                .layer(TimeoutLayer::new(
                    config.shaping.timeout.unwrap_or(DEFAULT_TIMEOUT),
                ))
                .option_layer(rate_limit)
                .option_layer(
                    config
                        .shaping
                        .deduplicate_query
                        .unwrap_or_default()
                        .then(QueryDeduplicationLayer::default),
                )
                .map_request(move |mut req: SubgraphRequest| {
                    if let Some(compression) = config.shaping.compression {
                        let compression_header_val = HeaderValue::from_str(&compression.to_string()).expect("compression is manually implemented and already have the right values; qed");
                        req.subgraph_request.headers_mut().insert(CONTENT_ENCODING, compression_header_val);
                    }
                    req
                })
                .buffered()
                .service(service)
                .boxed()
        } else {
            service
        }
    }

    fn connector_request_service(
        &self,
        service: crate::services::connector::request_service::BoxService,
        source_name: String,
    ) -> crate::services::connector::request_service::BoxService {
        let all_config = self.config.connector.all.as_ref();
        let source_config = self.config.connector.sources.get(&source_name).cloned();
        let final_config = Self::merge_config(all_config, source_config.as_ref());

        if let Some(config) = final_config {
            let rate_limit = config.global_rate_limit.as_ref().map(|rate_limit_conf| {
                self.rate_limit_sources
                    .lock()
                    .entry(source_name.clone())
                    .or_insert_with(|| {
                        RateLimitLayer::new(
                            rate_limit_conf.capacity.into(),
                            rate_limit_conf.interval,
                        )
                    })
                    .clone()
            });

            ServiceBuilder::new()
                .map_future_with_request_data(
                    |req: &Request| req.key.clone(),
                    move |response_key, future| {
                        async {
                            let response: Result<Response, BoxError> = future.await;
                            match response {
                                Ok(ok) => Ok(ok),
                                Err(err) if err.is::<Elapsed>() => {
                                    let response = Response::error_new(
                                        Error::GatewayTimeout,
                                        "Your request has been timed out",
                                        response_key,
                                    );
                                    Ok(response)
                                }
                               Err(err) if err.is::<Overloaded>() => {
                                    let response = Response::error_new(
                                        Error::RateLimited,
                                        "Your request has been rate limited",
                                        response_key,
                                    );
                                    Ok(response)
                                }
                                Err(err) => Err(err),
                            }
                        }
                    },
                )
                .load_shed()
                .layer(TimeoutLayer::new(
                    config.timeout.unwrap_or(DEFAULT_TIMEOUT),
                ))
                .option_layer(rate_limit)
                .map_request(move |mut req: connector::request_service::Request| {
                    if let Some(compression) = config.compression {
                        let TransportRequest::Http(ref mut http_request) = req.transport_request;
                        let compression_header_val = HeaderValue::from_str(&compression.to_string()).expect("compression is manually implemented and already have the right values; qed");
                        http_request.inner.headers_mut().insert(CONTENT_ENCODING, compression_header_val);
                    }
                    req
                })
                .buffered()
                .service(service)
                .boxed()
        } else {
            service
        }
    }
}

impl TrafficShaping {
    fn merge_config<T: Merge + Clone>(
        all_config: Option<&T>,
        subgraph_config: Option<&T>,
    ) -> Option<T> {
        let merged_subgraph_config = subgraph_config.map(|c| c.merge(all_config));
        merged_subgraph_config.or_else(|| all_config.cloned())
    }

    pub(crate) fn subgraph_client_config(
        &self,
        service_name: &str,
    ) -> crate::configuration::shared::Client {
        Self::merge_config(
            self.config.all.as_ref(),
            self.config.subgraphs.get(service_name),
        )
        .map(|config| crate::configuration::shared::Client {
            experimental_http2: config.shaping.experimental_http2,
            dns_resolution_strategy: config.shaping.dns_resolution_strategy,
        })
        .unwrap_or_default()
    }

    pub(crate) fn connector_client_config(
        &self,
        source_name: &str,
    ) -> crate::configuration::shared::Client {
        let source_config = self.config.connector.sources.get(source_name).cloned();
        Self::merge_config(self.config.connector.all.as_ref(), source_config.as_ref())
            .map(|config| crate::configuration::shared::Client {
                experimental_http2: config.experimental_http2,
                dns_resolution_strategy: config.dns_resolution_strategy,
            })
            .unwrap_or_default()
    }
}

register_private_plugin!("apollo", "traffic_shaping", TrafficShaping);

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use apollo_compiler::name;
    use apollo_federation::connectors::ConnectId;
    use apollo_federation::connectors::ConnectSpec;
    use apollo_federation::connectors::Connector;
    use apollo_federation::connectors::HttpJsonTransport;
    use apollo_federation::connectors::JSONSelection;
    use apollo_federation::connectors::SourceName;
    use apollo_federation::connectors::runtime::http_json_transport::HttpRequest;
    use apollo_federation::connectors::runtime::key::ResponseKey;
    use bytes::Bytes;
    use http::HeaderMap;
    use maplit::hashmap;
    use once_cell::sync::Lazy;
    use serde_json_bytes::ByteString;
    use serde_json_bytes::Value;
    use serde_json_bytes::json;
    use tower::Service;

    use super::*;
    use crate::Configuration;
    use crate::Context;
    use crate::json_ext::Object;
    use crate::plugin::DynPlugin;
    use crate::plugin::test::MockConnector;
    use crate::plugin::test::MockRouterService;
    use crate::plugin::test::MockSubgraph;
    use crate::query_planner::QueryPlannerService;
    use crate::router_factory::create_plugins;
    use crate::services::HasSchema;
    use crate::services::PluggableSupergraphServiceBuilder;
    use crate::services::RouterRequest;
    use crate::services::RouterResponse;
    use crate::services::SupergraphRequest;
    use crate::services::connector::request_service::Request as ConnectorRequest;
    use crate::services::layers::persisted_queries::PersistedQueryLayer;
    use crate::services::layers::query_analysis::QueryAnalysisLayer;
    use crate::services::router;
    use crate::services::router::service::RouterCreator;
    use crate::spec::Schema;

    static EXPECTED_RESPONSE: Lazy<Bytes> = Lazy::new(|| {
        Bytes::from_static(r#"{"data":{"topProducts":[{"upc":"1","name":"Table","reviews":[{"id":"1","product":{"name":"Table"},"author":{"id":"1","name":"Ada Lovelace"}},{"id":"4","product":{"name":"Table"},"author":{"id":"2","name":"Alan Turing"}}]},{"upc":"2","name":"Couch","reviews":[{"id":"2","product":{"name":"Couch"},"author":{"id":"1","name":"Ada Lovelace"}}]}]}}"#.as_bytes())
    });

    static VALID_QUERY: &str = r#"query TopProducts($first: Int) { topProducts(first: $first) { upc name reviews { id product { name } author { id name } } } }"#;

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

        assert_eq!(response, body);
    }

    async fn build_mock_router_with_variable_dedup_optimization(
        plugin: Box<dyn DynPlugin>,
    ) -> router::BoxService {
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

        let schema = include_str!(
            "../../../../apollo-router-benchmarks/benches/fixtures/supergraph.graphql"
        );

        let config: Configuration = serde_yaml::from_str(
            r#"
        traffic_shaping:
            deduplicate_variables: true
        supergraph:
            # TODO(@goto-bus-stop): need to update the mocks and remove this, #6013
            generate_query_fragments: false
        "#,
        )
        .unwrap();

        let config = Arc::new(config);
        let schema = Arc::new(Schema::parse(schema, &config).unwrap());
        let planner = QueryPlannerService::new(schema.clone(), config.clone())
            .await
            .unwrap();
        let subgraph_schemas = Arc::new(
            planner
                .subgraph_schemas()
                .iter()
                .map(|(k, v)| (k.clone(), v.schema.clone()))
                .collect(),
        );

        let mut builder =
            PluggableSupergraphServiceBuilder::new(planner).with_configuration(config.clone());

        let plugins = Arc::new(
            create_plugins(
                &config,
                &schema,
                subgraph_schemas,
                None,
                Some(vec![(APOLLO_TRAFFIC_SHAPING.to_string(), plugin)]),
                Default::default(),
            )
            .await
            .expect("create plugins should work"),
        );
        builder = builder.with_plugins(plugins);

        let builder = builder
            .with_subgraph_service("accounts", account_service.clone())
            .with_subgraph_service("reviews", review_service.clone())
            .with_subgraph_service("products", product_service.clone());

        let supergraph_creator = builder.build().await.expect("should build");

        RouterCreator::new(
            QueryAnalysisLayer::new(supergraph_creator.schema(), Default::default()).await,
            Arc::new(PersistedQueryLayer::new(&Default::default()).await.unwrap()),
            Arc::new(supergraph_creator),
            Arc::new(Configuration::default()),
        )
        .await
        .unwrap()
        .make()
        .boxed()
    }

    async fn get_traffic_shaping_plugin(config: &serde_json::Value) -> Box<dyn DynPlugin> {
        // Build a traffic shaping plugin
        crate::plugin::plugins()
            .find(|factory| factory.name == APOLLO_TRAFFIC_SHAPING)
            .expect("Plugin not found")
            .create_instance_without_schema(config)
            .await
            .expect("Plugin not created")
    }

    fn get_fake_connector_request(
        headers: Option<HeaderMap<HeaderValue>>,
        data: String,
    ) -> ConnectorRequest {
        let context = Context::default();
        let connector = Arc::new(Connector {
            spec: ConnectSpec::V0_1,
            id: ConnectId::new(
                "test_subgraph".into(),
                Some(SourceName::cast("test_sourcename")),
                name!(Query),
                name!(hello),
                None,
                0,
            ),
            transport: HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            },
            selection: JSONSelection::parse("$.data").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
            label: "test label".into(),
        });
        let key = ResponseKey::RootField {
            name: "hello".to_string(),
            inputs: Default::default(),
            selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
        };
        let mapping_problems = Default::default();

        let mut request_builder = http::Request::builder();
        if let Some(headers) = headers {
            for (header_name, header_value) in headers.iter() {
                request_builder = request_builder.header(header_name, header_value);
            }
        }
        let request = request_builder.body(data).unwrap();

        let http_request = HttpRequest {
            inner: request,
            debug: Default::default(),
        };

        ConnectorRequest {
            context,
            connector,
            transport_request: http_request.into(),
            key,
            mapping_problems,
            supergraph_request: Default::default(),
        }
    }

    #[tokio::test]
    async fn it_returns_valid_response_for_deduplicated_variables() {
        let config = serde_yaml::from_str::<serde_json::Value>(
            r#"
        deduplicate_variables: true
        "#,
        )
        .unwrap();
        // Build a traffic shaping plugin
        let plugin = get_traffic_shaping_plugin(&config).await;
        let router = build_mock_router_with_variable_dedup_optimization(plugin).await;
        execute_router_test(VALID_QUERY, &EXPECTED_RESPONSE, router).await;
    }

    #[tokio::test]
    async fn it_add_correct_headers_for_compression() {
        let config = serde_yaml::from_str::<serde_json::Value>(
            r#"
        subgraphs:
            test:
                compression: gzip
        "#,
        )
        .unwrap();

        let plugin = get_traffic_shaping_plugin(&config).await;
        let request = SubgraphRequest::fake_builder().build();

        let test_service = MockSubgraph::new(HashMap::new()).map_request(|req: SubgraphRequest| {
            assert_eq!(
                req.subgraph_request
                    .headers()
                    .get(&CONTENT_ENCODING)
                    .unwrap(),
                HeaderValue::from_static("gzip")
            );

            req
        });

        let _response = plugin
            .subgraph_service("test", test_service.boxed())
            .oneshot(request)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn it_adds_correct_headers_for_compression_for_connector() {
        let config = serde_yaml::from_str::<serde_json::Value>(
            r#"
        connector:
            sources:
                test_subgraph.test_sourcename:
                    compression: gzip
        "#,
        )
        .unwrap();

        let plugin = get_traffic_shaping_plugin(&config).await;
        let request = get_fake_connector_request(None, "testing".to_string());

        let test_service =
            MockConnector::new(HashMap::new()).map_request(|req: ConnectorRequest| {
                let TransportRequest::Http(ref http_request) = req.transport_request;

                assert_eq!(
                    http_request.inner.headers().get(&CONTENT_ENCODING).unwrap(),
                    HeaderValue::from_static("gzip")
                );

                req
            });

        let _response = plugin
            .connector_request_service(
                test_service.boxed(),
                "test_subgraph.test_sourcename".to_string(),
            )
            .oneshot(request)
            .await
            .unwrap();
    }

    #[test]
    fn test_merge_config() {
        let config = serde_yaml::from_str::<Config>(
            r#"
        all:
          deduplicate_query: true
        subgraphs:
          products:
            deduplicate_query: false
        "#,
        )
        .unwrap();

        assert_eq!(TrafficShaping::merge_config::<Shaping>(None, None), None);
        assert_eq!(
            TrafficShaping::merge_config(config.all.as_ref(), None),
            config.all
        );
        assert_eq!(
            TrafficShaping::merge_config(config.all.as_ref(), config.subgraphs.get("products"))
                .as_ref(),
            config.subgraphs.get("products")
        );

        assert_eq!(
            TrafficShaping::merge_config(None, config.subgraphs.get("products")).as_ref(),
            config.subgraphs.get("products")
        );
    }

    #[test]
    fn test_merge_http2_all() {
        let config = serde_yaml::from_str::<Config>(
            r#"
        all:
          experimental_http2: disable
        subgraphs:
          products:
            experimental_http2: enable
          reviews:
            experimental_http2: disable
        router:
          timeout: 65s
        "#,
        )
        .unwrap();

        assert!(
            TrafficShaping::merge_config(config.all.as_ref(), config.subgraphs.get("products"))
                .unwrap()
                .shaping
                .experimental_http2
                .unwrap()
                == Http2Config::Enable
        );
        assert!(
            TrafficShaping::merge_config(config.all.as_ref(), config.subgraphs.get("reviews"))
                .unwrap()
                .shaping
                .experimental_http2
                .unwrap()
                == Http2Config::Disable
        );
        assert!(
            TrafficShaping::merge_config(config.all.as_ref(), None)
                .unwrap()
                .shaping
                .experimental_http2
                .unwrap()
                == Http2Config::Disable
        );
    }

    #[tokio::test]
    async fn test_subgraph_client_config() {
        let config = serde_yaml::from_str::<Config>(
            r#"
        all:
          experimental_http2: disable
          dns_resolution_strategy: ipv6_only
        subgraphs:
          products:
            experimental_http2: enable
            dns_resolution_strategy: ipv6_then_ipv4
          reviews:
            experimental_http2: disable
            dns_resolution_strategy: ipv4_only
        router:
          timeout: 65s
        "#,
        )
        .unwrap();

        let shaping_config = TrafficShaping::new(PluginInit::fake_builder().config(config).build())
            .await
            .unwrap();

        assert_eq!(
            shaping_config.subgraph_client_config("products"),
            crate::configuration::shared::Client {
                experimental_http2: Some(Http2Config::Enable),
                dns_resolution_strategy: Some(DnsResolutionStrategy::Ipv6ThenIpv4),
            },
        );
        assert_eq!(
            shaping_config.subgraph_client_config("reviews"),
            crate::configuration::shared::Client {
                experimental_http2: Some(Http2Config::Disable),
                dns_resolution_strategy: Some(DnsResolutionStrategy::Ipv4Only),
            },
        );
        assert_eq!(
            shaping_config.subgraph_client_config("this_doesnt_exist"),
            crate::configuration::shared::Client {
                experimental_http2: Some(Http2Config::Disable),
                dns_resolution_strategy: Some(DnsResolutionStrategy::Ipv6Only),
            },
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_rate_limit_subgraph_requests() {
        let config = serde_yaml::from_str::<serde_json::Value>(
            r#"
        subgraphs:
            test:
                global_rate_limit:
                    capacity: 1
                    interval: 100ms
                timeout: 500ms
        "#,
        )
        .unwrap();

        let plugin = get_traffic_shaping_plugin(&config).await;

        let test_service = MockSubgraph::new(hashmap! {
            graphql::Request::default() => graphql::Response::default()
        });

        let mut svc = plugin.subgraph_service("test", test_service.boxed());

        assert!(
            svc.ready()
                .await
                .expect("it is ready")
                .call(SubgraphRequest::fake_builder().build())
                .await
                .unwrap()
                .response
                .body()
                .errors
                .is_empty()
        );
        let response = svc
            .ready()
            .await
            .expect("it is ready")
            .call(SubgraphRequest::fake_builder().build())
            .await
            .expect("it responded");

        assert_eq!(StatusCode::SERVICE_UNAVAILABLE, response.response.status());

        tokio::time::sleep(Duration::from_millis(300)).await;

        assert!(
            svc.ready()
                .await
                .expect("it is ready")
                .call(SubgraphRequest::fake_builder().build())
                .await
                .unwrap()
                .response
                .body()
                .errors
                .is_empty()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_rate_limit_connector_requests() {
        let config = serde_yaml::from_str::<serde_json::Value>(
            r#"
        connector:
            sources:
                test_subgraph.test_sourcename:
                    global_rate_limit:
                        capacity: 1
                        interval: 100ms
                    timeout: 500ms
        "#,
        )
        .unwrap();

        let plugin = get_traffic_shaping_plugin(&config).await;
        let request = get_fake_connector_request(None, "testing".to_string());

        let test_service = MockConnector::new(hashmap! {
            "test_request".into() => "test_request".into()
        });

        let mut svc = plugin.connector_request_service(
            test_service.boxed(),
            "test_subgraph.test_sourcename".to_string(),
        );

        assert!(
            svc.ready()
                .await
                .expect("it is ready")
                .call(request)
                .await
                .unwrap()
                .transport_result
                .is_ok()
        );

        let request = get_fake_connector_request(None, "testing".to_string());
        let response = svc
            .ready()
            .await
            .expect("it is ready")
            .call(request)
            .await
            .expect("it responded");

        assert!(response.transport_result.is_err());
        assert!(matches!(
            response.transport_result.err().unwrap(),
            Error::RateLimited
        ));

        tokio::time::sleep(Duration::from_millis(300)).await;

        let request = get_fake_connector_request(None, "testing".to_string());
        assert!(
            svc.ready()
                .await
                .expect("it is ready")
                .call(request)
                .await
                .unwrap()
                .transport_result
                .is_ok()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_rate_limit_router_requests() {
        let config = serde_yaml::from_str::<serde_json::Value>(
            r#"
        router:
            global_rate_limit:
                capacity: 1
                interval: 100ms
            timeout: 500ms
        "#,
        )
        .unwrap();

        let plugin = get_traffic_shaping_plugin(&config).await;
        let mut mock_service = MockRouterService::new();

        mock_service.expect_call().times(0..3).returning(|_| {
            Ok(RouterResponse::fake_builder()
                .data(json!({ "test": 1234_u32 }))
                .build()
                .unwrap())
        });
        mock_service
            .expect_clone()
            .returning(MockRouterService::new);

        // let mut svc = plugin.router_service(mock_service.clone().boxed());
        let mut svc = plugin.router_service(mock_service.boxed());

        let response: RouterResponse = svc
            .ready()
            .await
            .expect("it is ready")
            .call(RouterRequest::fake_builder().build().unwrap())
            .await
            .unwrap();
        assert_eq!(StatusCode::OK, response.response.status());

        let response: RouterResponse = svc
            .ready()
            .await
            .expect("it is ready")
            .call(RouterRequest::fake_builder().build().unwrap())
            .await
            .unwrap();
        assert_eq!(StatusCode::SERVICE_UNAVAILABLE, response.response.status());
        let j: serde_json::Value = serde_json::from_slice(
            &router::body::into_bytes(response.response)
                .await
                .expect("we have a body"),
        )
        .expect("our body is valid json");
        assert_eq!(
            "Your request has been rate limited",
            j["errors"][0]["message"]
        );

        tokio::time::sleep(Duration::from_millis(300)).await;

        let response: RouterResponse = svc
            .ready()
            .await
            .expect("it is ready")
            .call(RouterRequest::fake_builder().build().unwrap())
            .await
            .unwrap();
        assert_eq!(StatusCode::OK, response.response.status());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_timeout_router_requests() {
        let config = serde_yaml::from_str::<serde_json::Value>(
            r#"
        router:
            timeout: 1ns
        "#,
        )
        .unwrap();

        let plugin = get_traffic_shaping_plugin(&config).await;

        let svc = ServiceBuilder::new()
            .service_fn(move |_req: router::Request| async {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                RouterResponse::fake_builder()
                    .data(json!({ "test": 1234_u32 }))
                    .build()
            })
            .boxed();

        let mut rs = plugin.router_service(svc);

        let response: RouterResponse = rs
            .ready()
            .await
            .expect("it is ready")
            .call(RouterRequest::fake_builder().build().unwrap())
            .await
            .unwrap();
        assert_eq!(StatusCode::GATEWAY_TIMEOUT, response.response.status());
        let j: serde_json::Value = serde_json::from_slice(
            &crate::services::router::body::into_bytes(response.response)
                .await
                .expect("we have a body"),
        )
        .expect("our body is valid json");
        assert_eq!("Your request has been timed out", j["errors"][0]["message"]);
    }
}
