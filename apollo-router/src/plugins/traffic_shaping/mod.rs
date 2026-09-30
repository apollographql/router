//! Traffic shaping plugin
//!
//! Currently includes:
//! * Query deduplication
//! * Timeout
//! * Compression
//! * Rate limiting
//!
mod admission;
mod deduplication;

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::time::Duration;

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
use tower::util::MapRequestLayer;
use tower::util::option_layer;

use self::admission::ConnectorSourceAdmissionLayer;
use self::admission::SubgraphAdmissionLayer;
use self::deduplication::QueryDeduplicationLayer;
use crate::configuration::shared::DnsResolutionStrategy;
use crate::configuration::shared::default_pool_idle_timeout;
use crate::graphql;
use crate::layers::DEFAULT_BUFFER_SIZE;
use crate::layers::OptionLayer;
use crate::layers::ServiceBuilderExt;
use crate::layers::unconstrained_buffer::UnconstrainedBufferLayer;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::services::RouterResponse;
use crate::services::SubgraphRequest;
use crate::services::connector;
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
    http2: Option<Http2Config>,
    /// DNS resolution strategy for subgraphs
    dns_resolution_strategy: Option<DnsResolutionStrategy>,
    /// Specify a timeout for idle sockets being kept-alive in the client's connection pool
    #[serde(
        deserialize_with = "humantime_serde::deserialize",
        default = "default_pool_idle_timeout"
    )]
    #[schemars(with = "Option<String>", default = "default_pool_idle_timeout")]
    pool_idle_timeout: Option<Duration>,
    /// Configure the interval for HTTP/2 keep-alive pings. Requires HTTP/2 to be enabled. If
    /// unset (the default), keep-alive pings are disabled.
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "Option<String>", default)]
    experimental_http2_keep_alive_interval: Option<Duration>,
    /// Configure the timeout for HTTP/2 keep-alive pings. Requires HTTP/2 to be enabled and
    /// `experimental_http2_keep_alive_interval` to be set. Defaults to 20 seconds.
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "Option<String>", default)]
    experimental_http2_keep_alive_timeout: Option<Duration>,
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
                http2: self.http2.as_ref().or(fallback.http2.as_ref()).cloned(),
                dns_resolution_strategy: self
                    .dns_resolution_strategy
                    .as_ref()
                    .or(fallback.dns_resolution_strategy.as_ref())
                    .cloned(),
                pool_idle_timeout: self
                    .pool_idle_timeout
                    .as_ref()
                    .or(fallback.pool_idle_timeout.as_ref())
                    .cloned(),
                experimental_http2_keep_alive_interval: self
                    .experimental_http2_keep_alive_interval
                    .as_ref()
                    .or(fallback.experimental_http2_keep_alive_interval.as_ref())
                    .cloned(),
                experimental_http2_keep_alive_timeout: self
                    .experimental_http2_keep_alive_timeout
                    .as_ref()
                    .or(fallback.experimental_http2_keep_alive_timeout.as_ref())
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
    /// Specify a timeout for idle sockets being kept-alive in the client's connection pool
    #[serde(
        deserialize_with = "humantime_serde::deserialize",
        default = "default_pool_idle_timeout"
    )]
    #[schemars(with = "Option<String>", default = "default_pool_idle_timeout")]
    pool_idle_timeout: Option<Duration>,
    /// Configure the interval for HTTP/2 keep-alive pings. Requires HTTP/2 to be enabled. If
    /// unset (the default), keep-alive pings are disabled.
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "Option<String>", default)]
    experimental_http2_keep_alive_interval: Option<Duration>,
    /// Configure the timeout for HTTP/2 keep-alive pings. Requires HTTP/2 to be enabled and
    /// `experimental_http2_keep_alive_interval` to be set. Defaults to 20 seconds.
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "Option<String>", default)]
    experimental_http2_keep_alive_timeout: Option<Duration>,
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
                pool_idle_timeout: self
                    .pool_idle_timeout
                    .as_ref()
                    .or(fallback.pool_idle_timeout.as_ref())
                    .cloned(),
                experimental_http2_keep_alive_interval: self
                    .experimental_http2_keep_alive_interval
                    .as_ref()
                    .or(fallback.experimental_http2_keep_alive_interval.as_ref())
                    .cloned(),
                experimental_http2_keep_alive_timeout: self
                    .experimental_http2_keep_alive_timeout
                    .as_ref()
                    .or(fallback.experimental_http2_keep_alive_timeout.as_ref())
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

#[apollo_configuration::configuration]
#[schemars(rename = "TrafficShapingConfig")]
// FIXME: This struct is pub(crate) because we need its configuration in the query planner service.
// Remove this once the configuration yml changes.
/// Configuration for the traffic shaping plugin
pub(crate) struct Config {
    /// Applied at the router level
    #[config(skip_validate)]
    router: Option<RouterShaping>,
    /// Applied on all subgraphs
    #[config(skip_validate)]
    all: Option<SubgraphShaping>,
    /// Applied on specific subgraphs
    #[config(skip_validate)]
    subgraphs: HashMap<String, SubgraphShaping>,
    /// Applied on specific subgraphs
    #[config(skip_validate)]
    connector: ConnectorsShapingConfig,
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

    fn router_service(&self, service: router::BoxCloneService) -> router::BoxCloneService {
        // NB: consider each triplet (map_future_with_request_data, load_shed, layer) as a unit of
        //  behavior.
        // The outer buffer before the first load_shed() is required for correct cooperative-
        // scheduling behaviour: without it, Tokio's budget can cause poll_ready to return
        // Pending spuriously and the load_shed would emit false Overloaded errors.
        ServiceBuilder::new()
            .buffered()
            .map_future_with_request_data(
                |req: &router::Request| req.context.clone(),
                move |ctx, future| async {
                    let response: Result<RouterResponse, BoxError> = future.await;
                    if matches!(response, Err(ref err) if err.is::<Elapsed>()) {
                        Ok(RouterResponse::error_builder()
                            .status_code(StatusCode::GATEWAY_TIMEOUT)
                            .error(gateway_timeout_error())
                            .context(ctx)
                            .build()
                            .expect("should build overloaded response"))
                    } else {
                        response
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
                move |ctx, future| async {
                    let response: Result<RouterResponse, BoxError> = future.await;
                    if matches!(response, Err(ref err) if err.is::<Overloaded>()) {
                        Ok(RouterResponse::error_builder()
                            .status_code(StatusCode::SERVICE_UNAVAILABLE)
                            .error(concurrency_limit_error())
                            .context(ctx)
                            .build()
                            .expect("should build overloaded response"))
                    } else {
                        response
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
                move |ctx, future| async {
                    let response: Result<RouterResponse, BoxError> = future.await;
                    if matches!(response, Err(ref err) if err.is::<Overloaded>()) {
                        Ok(RouterResponse::error_builder()
                            .status_code(StatusCode::SERVICE_UNAVAILABLE)
                            .error(rate_limit_error())
                            .context(ctx)
                            .build()
                            .expect("should build overloaded response"))
                    } else {
                        response
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
            .boxed_clone()
    }

    fn subgraph_service(
        &self,
        name: &str,
        service: subgraph::BoxCloneService,
    ) -> subgraph::BoxCloneService {
        ServiceBuilder::new()
            .layer(self.subgraph_admission_layer(name))
            .layer(self.subgraph_timeout_layer(name))
            .layer(self.subgraph_deduplication_layer(name))
            .layer(self.subgraph_compression_layer(name))
            .layer(self.subgraph_backpressure_buffer_layer(name))
            .service(service)
            .boxed_clone()
    }

    fn connector_request_service(
        &self,
        service: connector::request_service::BoxCloneService,
        source_name: String,
    ) -> connector::request_service::BoxCloneService {
        ServiceBuilder::new()
            .layer(self.connector_source_admission_layer(&source_name))
            .layer(self.connector_source_timeout_layer(&source_name))
            .layer(self.connector_source_compression_layer(&source_name))
            .layer(self.connector_source_backpressure_buffer_layer(&source_name))
            .service(service)
            .boxed_clone()
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
            http2: config.shaping.http2,
            dns_resolution_strategy: config.shaping.dns_resolution_strategy,
            pool_idle_timeout: config.shaping.pool_idle_timeout,
            experimental_http2_keep_alive_interval: config
                .shaping
                .experimental_http2_keep_alive_interval,
            experimental_http2_keep_alive_timeout: config
                .shaping
                .experimental_http2_keep_alive_timeout,
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
                http2: config.experimental_http2,
                dns_resolution_strategy: config.dns_resolution_strategy,
                pool_idle_timeout: config.pool_idle_timeout,
                experimental_http2_keep_alive_interval: config
                    .experimental_http2_keep_alive_interval,
                experimental_http2_keep_alive_timeout: config.experimental_http2_keep_alive_timeout,
            })
            .unwrap_or_default()
    }
}

/// The layers traffic shaping applies to each subgraph and connector source.
///
/// A target with no traffic shaping configuration, neither its own block nor `all`, gets an
/// identity layer from every constructor. The layers go in this order, from the outside in:
///
/// 1. admission: outer buffer, error mapping, load shedding and rate limit
/// 2. timeout
/// 3. deduplication (subgraphs only)
/// 4. compression
/// 5. backpressure buffer
///
/// Admission renders the errors of the timeout beneath it, so the timeout must stay below
/// admission. The [`admission`] module explains why admission is one layer.
impl TrafficShaping {
    /// This subgraph's shaping: its own block merged over `all`, or `all` alone.
    fn subgraph_shaping(&self, name: &str) -> Option<Shaping> {
        Self::merge_config(self.config.all.as_ref(), self.config.subgraphs.get(name))
            .map(|config| config.shaping)
    }

    /// This connector source's shaping: its own block merged over `all`, or `all` alone.
    fn connector_source_shaping(&self, source: &str) -> Option<ConnectorShaping> {
        Self::merge_config(
            self.config.connector.all.as_ref(),
            self.config.connector.sources.get(source),
        )
    }

    /// Caches the rate configuration for this target for the plugin's lifetime. Each service
    /// built from the layer has an independent counter.
    fn cached_rate_limit_layer(
        rate_limits: &Mutex<HashMap<String, RateLimitLayer>>,
        key: &str,
        conf: Option<&RateLimitConf>,
    ) -> Option<RateLimitLayer> {
        conf.map(|conf| {
            rate_limits
                .lock()
                .entry(key.to_string())
                .or_insert_with(|| RateLimitLayer::new(conf.capacity.into(), conf.interval))
                .clone()
        })
    }

    /// Returns a layer that admits or rejects requests to this subgraph. A request over the
    /// subgraph's rate limit is answered with a `503`, and a timeout raised beneath this layer
    /// with a `504`.
    pub(crate) fn subgraph_admission_layer(
        &self,
        name: &str,
    ) -> OptionLayer<SubgraphAdmissionLayer> {
        option_layer(self.subgraph_shaping(name).map(|shaping| {
            SubgraphAdmissionLayer::new(Self::cached_rate_limit_layer(
                &self.rate_limit_subgraphs,
                name,
                shaping.global_rate_limit.as_ref(),
            ))
        }))
    }

    /// Returns a layer that fails a request to this subgraph once it runs past the subgraph's
    /// timeout (30 seconds by default).
    pub(crate) fn subgraph_timeout_layer(&self, name: &str) -> OptionLayer<TimeoutLayer> {
        option_layer(
            self.subgraph_shaping(name)
                .map(|shaping| TimeoutLayer::new(shaping.timeout.unwrap_or(DEFAULT_TIMEOUT))),
        )
    }

    /// Returns a layer that lets identical in-flight queries to this subgraph share one
    /// request, when `deduplicate_query` is enabled for it.
    pub(crate) fn subgraph_deduplication_layer(
        &self,
        name: &str,
    ) -> OptionLayer<QueryDeduplicationLayer> {
        option_layer(self.subgraph_shaping(name).and_then(|shaping| {
            shaping
                .deduplicate_query
                .unwrap_or_default()
                .then(QueryDeduplicationLayer::default)
        }))
    }

    /// Returns a layer that sets `Content-Encoding` on requests to this subgraph, when
    /// compression is configured for it.
    pub(crate) fn subgraph_compression_layer(
        &self,
        name: &str,
    ) -> OptionLayer<MapRequestLayer<impl Fn(SubgraphRequest) -> SubgraphRequest + Clone + use<>>>
    {
        option_layer(
            self.subgraph_shaping(name)
                .and_then(|shaping| shaping.compression)
                .map(|compression| {
                    let encoding = content_encoding(compression);
                    MapRequestLayer::new(move |mut req: SubgraphRequest| {
                        req.subgraph_request
                            .headers_mut()
                            .insert(CONTENT_ENCODING, encoding.clone());
                        req
                    })
                }),
        )
    }

    /// Returns the buffer beneath this subgraph's shaping layers, which gives the rate limit
    /// and load shedding above it a backpressure surface.
    pub(crate) fn subgraph_backpressure_buffer_layer(
        &self,
        name: &str,
    ) -> OptionLayer<UnconstrainedBufferLayer<subgraph::Request>> {
        option_layer(
            self.subgraph_shaping(name)
                .map(|_| UnconstrainedBufferLayer::new(DEFAULT_BUFFER_SIZE)),
        )
    }

    /// Returns a layer that admits or rejects requests to this connector source, keyed by
    /// `<subgraph name>.<source name>`. A request over the source's rate limit is answered with
    /// a rate-limited error, and a timeout raised beneath this layer with a gateway-timeout
    /// error.
    pub(crate) fn connector_source_admission_layer(
        &self,
        source: &str,
    ) -> OptionLayer<ConnectorSourceAdmissionLayer> {
        option_layer(self.connector_source_shaping(source).map(|shaping| {
            ConnectorSourceAdmissionLayer::new(Self::cached_rate_limit_layer(
                &self.rate_limit_sources,
                source,
                shaping.global_rate_limit.as_ref(),
            ))
        }))
    }

    /// Returns a layer that fails a request to this connector source once it runs past the
    /// source's timeout (30 seconds by default).
    pub(crate) fn connector_source_timeout_layer(&self, source: &str) -> OptionLayer<TimeoutLayer> {
        option_layer(
            self.connector_source_shaping(source)
                .map(|shaping| TimeoutLayer::new(shaping.timeout.unwrap_or(DEFAULT_TIMEOUT))),
        )
    }

    /// Returns a layer that sets `Content-Encoding` on HTTP requests to this connector source,
    /// when compression is configured for it.
    pub(crate) fn connector_source_compression_layer(
        &self,
        source: &str,
    ) -> OptionLayer<
        MapRequestLayer<
            impl Fn(connector::request_service::Request) -> connector::request_service::Request
            + Clone
            + use<>,
        >,
    > {
        option_layer(
            self.connector_source_shaping(source)
                .and_then(|shaping| shaping.compression)
                .map(|compression| {
                    let encoding = content_encoding(compression);
                    MapRequestLayer::new(move |mut req: connector::request_service::Request| {
                        if let TransportRequest::Http(ref mut http_request) = req.transport_request
                        {
                            http_request
                                .inner
                                .headers_mut()
                                .insert(CONTENT_ENCODING, encoding.clone());
                        }
                        req
                    })
                }),
        )
    }

    /// Returns the buffer beneath this connector source's shaping layers, which gives the rate
    /// limit and load shedding above it a backpressure surface.
    pub(crate) fn connector_source_backpressure_buffer_layer(
        &self,
        source: &str,
    ) -> OptionLayer<UnconstrainedBufferLayer<connector::request_service::Request>> {
        option_layer(
            self.connector_source_shaping(source)
                .map(|_| UnconstrainedBufferLayer::new(DEFAULT_BUFFER_SIZE)),
        )
    }
}

/// The `Content-Encoding` header value for `compression`.
fn content_encoding(compression: Compression) -> HeaderValue {
    HeaderValue::from_str(&compression.to_string())
        .expect("compression is manually implemented and already have the right values; qed")
}

fn concurrency_limit_error() -> graphql::Error {
    graphql::Error::builder()
        .message("Your request has been concurrency limited")
        .extension_code("REQUEST_CONCURRENCY_LIMITED")
        .build()
}

fn gateway_timeout_error() -> graphql::Error {
    graphql::Error::builder()
        .message("Your request has been timed out")
        .extension_code("GATEWAY_TIMEOUT")
        .build()
}

fn rate_limit_error() -> graphql::Error {
    graphql::Error::builder()
        .message("Your request has been rate limited")
        .extension_code("REQUEST_RATE_LIMITED")
        .build()
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
    use apollo_federation::connectors::runtime::errors::Error;
    use apollo_federation::connectors::runtime::http_json_transport::HttpRequest;
    use apollo_federation::connectors::runtime::key::ResponseKey;
    use bytes::Bytes;
    use http::HeaderMap;
    use maplit::hashmap;
    use once_cell::sync::Lazy;
    use serde_json_bytes::ByteString;
    use serde_json_bytes::Value;
    use serde_json_bytes::json;
    use tokio::task::JoinSet;
    use tokio::time::sleep;
    use tower::Service;

    use super::*;
    use crate::Configuration;
    use crate::Context;
    use crate::json_ext::Object;
    use crate::pipeline::build_apq_expander;
    use crate::pipeline::build_query_plan_cache;
    use crate::pipeline::build_supergraph_pipeline;
    use crate::pipeline::connect_apq_redis;
    use crate::pipeline::connect_query_plan_redis;
    use crate::pipeline::create_plugins;
    use crate::plugin::DynPlugin;
    use crate::plugin::test::MockConnector;
    use crate::plugin::test::MockSubgraph;
    use crate::query_planner::QueryPlannerService;
    use crate::services::RouterRequest;
    use crate::services::RouterResponse;
    use crate::services::SupergraphRequest;
    use crate::services::connector::request_service::Request as ConnectorRequest;
    use crate::services::layers::persisted_queries::PersistedQueryExpander;
    use crate::services::router;
    use crate::spec::Schema;

    static EXPECTED_RESPONSE: Lazy<Bytes> = Lazy::new(|| {
        Bytes::from_static(r#"{"data":{"topProducts":[{"upc":"1","name":"Table","reviews":[{"id":"1","product":{"name":"Table"},"author":{"id":"1","name":"Ada Lovelace"}},{"id":"4","product":{"name":"Table"},"author":{"id":"2","name":"Alan Turing"}}]},{"upc":"2","name":"Couch","reviews":[{"id":"2","product":{"name":"Couch"},"author":{"id":"1","name":"Ada Lovelace"}}]}]}}"#.as_bytes())
    });

    static VALID_QUERY: &str = r#"query TopProducts($first: Int) { topProducts(first: $first) { upc name reviews { id product { name } author { id name } } } }"#;

    async fn execute_router_test(
        query: &str,
        body: &Bytes,
        mut router_service: router::BoxCloneService,
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
    ) -> router::BoxCloneService {
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
        supergraph:
            # TODO(@goto-bus-stop): need to update the mocks and remove this, #6013
            generate_query_fragments: false
        "#,
        )
        .unwrap();

        let config = Arc::new(config);
        let schema = Arc::new(Schema::parse(schema, &config).unwrap());

        let qp_arc = QueryPlannerService::create_planner(&schema, &config).unwrap();
        let subgraph_schemas = crate::query_planner::build_subgraph_schemas(&qp_arc);

        let query_parser_service =
            crate::pipeline::build_query_parsing_service(schema.clone(), config.clone());

        let plugins = Arc::new(
            create_plugins(
                &config,
                &schema,
                subgraph_schemas.clone(),
                None,
                Some(vec![
                    (APOLLO_TRAFFIC_SHAPING.to_string(), plugin),
                    // Replaces each subgraph's transport with a mock. Must be last so
                    // the traffic-shaping hooks under test still wrap the mocks.
                    (
                        "mocked_subgraphs".to_string(),
                        Box::new(crate::test_harness::MockedSubgraphs(hashmap! {
                            "accounts" => account_service,
                            "reviews" => review_service,
                            "products" => product_service,
                        })),
                    ),
                ]),
                Default::default(),
                None,
            )
            .await
            .expect("create plugins should work"),
        );

        for (_, plugin) in plugins.iter() {
            plugin.activate();
        }

        let query_plan_cache =
            build_query_plan_cache(&config, connect_query_plan_redis(&config).await.unwrap());
        let query_planner_service = crate::pipeline::build_query_planner_service(
            schema.clone(),
            config.clone(),
            qp_arc,
            subgraph_schemas.clone(),
            query_plan_cache,
        );

        let subgraph_services = crate::pipeline::build_subgraph_services(
            ["accounts", "reviews", "products"]
                .into_iter()
                .map(|name| {
                    (
                        name.to_string(),
                        crate::services::http::test_http_client_service(name),
                    )
                })
                .collect(),
            &plugins,
            &config,
        );
        let supergraph_service = build_supergraph_pipeline(
            query_planner_service,
            schema.clone(),
            subgraph_schemas,
            config.clone(),
            plugins.clone(),
            subgraph_services,
            Default::default(),
        );

        let apq_expander = build_apq_expander(&config, connect_apq_redis(&config).await.unwrap());
        crate::pipeline::build_router_service(
            supergraph_service,
            apq_expander,
            Arc::new(PersistedQueryExpander::new(&config).await.unwrap()),
            query_parser_service,
            schema,
            &config,
            plugins,
        )
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
            schema_subtypes_map: Default::default(),
            id: ConnectId::new(
                "test_subgraph".into(),
                Some(SourceName::cast("test_sourcename")),
                name!(Query),
                name!(hello),
                None,
                0,
            ),
            transport: Some(HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            }),
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
            output_type: None,
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
            operation: Default::default(),
        }
    }

    #[tokio::test]
    async fn it_returns_valid_response_for_deduplicated_variables() {
        // Variable deduplication is now unconditionally enabled, so an empty
        // traffic shaping config is sufficient.
        let config = serde_yaml::from_str::<serde_json::Value>("{}").unwrap();
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
            .subgraph_service("test", test_service.boxed_clone())
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
                let TransportRequest::Http(ref http_request) = req.transport_request else {
                    panic!("expected Http transport request");
                };

                assert_eq!(
                    http_request.inner.headers().get(&CONTENT_ENCODING).unwrap(),
                    HeaderValue::from_static("gzip")
                );

                req
            });

        let _response = plugin
            .connector_request_service(
                test_service.boxed_clone(),
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
          http2: disable
        subgraphs:
          products:
            http2: enable
          reviews:
            http2: disable
        router:
          timeout: 65s
        "#,
        )
        .unwrap();

        assert!(
            TrafficShaping::merge_config(config.all.as_ref(), config.subgraphs.get("products"))
                .unwrap()
                .shaping
                .http2
                .unwrap()
                == Http2Config::Enable
        );
        assert!(
            TrafficShaping::merge_config(config.all.as_ref(), config.subgraphs.get("reviews"))
                .unwrap()
                .shaping
                .http2
                .unwrap()
                == Http2Config::Disable
        );
        assert!(
            TrafficShaping::merge_config(config.all.as_ref(), None)
                .unwrap()
                .shaping
                .http2
                .unwrap()
                == Http2Config::Disable
        );
    }

    #[tokio::test]
    async fn test_subgraph_client_config() {
        let config = serde_yaml::from_str::<Config>(
            r#"
        all:
          http2: disable
          dns_resolution_strategy: ipv6_only
        subgraphs:
          products:
            http2: enable
            dns_resolution_strategy: ipv6_then_ipv4
          reviews:
            http2: disable
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
                http2: Some(Http2Config::Enable),
                dns_resolution_strategy: Some(DnsResolutionStrategy::Ipv6ThenIpv4),
                pool_idle_timeout: default_pool_idle_timeout(),
                ..Default::default()
            },
        );
        assert_eq!(
            shaping_config.subgraph_client_config("reviews"),
            crate::configuration::shared::Client {
                http2: Some(Http2Config::Disable),
                dns_resolution_strategy: Some(DnsResolutionStrategy::Ipv4Only),
                pool_idle_timeout: default_pool_idle_timeout(),
                ..Default::default()
            },
        );
        assert_eq!(
            shaping_config.subgraph_client_config("this_doesnt_exist"),
            crate::configuration::shared::Client {
                http2: Some(Http2Config::Disable),
                dns_resolution_strategy: Some(DnsResolutionStrategy::Ipv6Only),
                pool_idle_timeout: default_pool_idle_timeout(),
                ..Default::default()
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

        let mut svc = plugin.subgraph_service("test", test_service.boxed_clone());

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
            test_service.boxed_clone(),
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
        let (mock, mut handle) = tower_test::mock::pair::<RouterRequest, RouterResponse>();

        // First and third requests pass through; the second is rate-limited by the plugin and
        // never reaches the inner service.
        let driver = tokio::spawn(async move {
            for _ in 0..2 {
                let (_req, responder) = handle.next_request().await.unwrap();
                responder.send_response(
                    RouterResponse::fake_builder()
                        .data(json!({ "test": 1234_u32 }))
                        .build()
                        .unwrap(),
                );
            }
        });

        let mut svc = plugin.router_service(mock.boxed_clone());

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
        crate::plugin::test::await_mock_driver(driver).await;
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
            .boxed_clone();

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

    #[tokio::test]
    async fn test_subgraph_pool_idle_timeout_override_and_fallback() {
        let config = serde_yaml::from_str::<Config>(
            r#"
        all:
          pool_idle_timeout: 10s
        subgraphs:
          fast:
            pool_idle_timeout: 2s
          explicit_null:
            pool_idle_timeout: null
        router:
          timeout: 65s
        "#,
        )
        .unwrap();

        let shaping_config = TrafficShaping::new(PluginInit::fake_builder().config(config).build())
            .await
            .unwrap();

        assert_eq!(
            shaping_config
                .subgraph_client_config("fast")
                .pool_idle_timeout,
            Some(Duration::from_secs(2)),
            "subgraph-specific override should win"
        );

        assert_eq!(
            shaping_config
                .subgraph_client_config("explicit_null")
                .pool_idle_timeout,
            Some(Duration::from_secs(10)),
            "explicit null falls back to all"
        );

        assert_eq!(
            shaping_config
                .subgraph_client_config("unknown")
                .pool_idle_timeout,
            Some(Duration::from_secs(10)),
            "unknown subgraph falls back to all"
        );
    }

    #[tokio::test]
    async fn test_subgraph_pool_idle_timeout_null() {
        let config = serde_yaml::from_str::<Config>(
            r#"
        all:
          pool_idle_timeout: null
        subgraphs:
          explicit_value:
            pool_idle_timeout: 10s
          explicit_null:
            pool_idle_timeout: null
        router:
          timeout: 65s
        "#,
        )
        .unwrap();

        let shaping_config = TrafficShaping::new(PluginInit::fake_builder().config(config).build())
            .await
            .unwrap();

        assert_eq!(
            shaping_config
                .subgraph_client_config("explicit_value")
                .pool_idle_timeout,
            Some(Duration::from_secs(10)),
            "subgraph-specific override should win"
        );

        assert!(
            shaping_config
                .subgraph_client_config("unknown")
                .pool_idle_timeout
                .is_none(),
            "explicit null falls back to all"
        );

        assert!(
            shaping_config
                .subgraph_client_config("unknown")
                .pool_idle_timeout
                .is_none(),
            "unknown subgraph falls back to all"
        );
    }

    #[tokio::test]
    async fn test_connector_pool_idle_timeout_override_and_fallback() {
        let config = serde_yaml::from_str::<Config>(
            r#"
        connector:
          all:
            pool_idle_timeout: 20s
          sources:
            my_source:
              pool_idle_timeout: 3s
            explicit_null:
              pool_idle_timeout: null
        router:
          timeout: 65s
        "#,
        )
        .unwrap();

        let shaping_config = TrafficShaping::new(PluginInit::fake_builder().config(config).build())
            .await
            .unwrap();

        assert_eq!(
            shaping_config
                .connector_client_config("my_source")
                .pool_idle_timeout,
            Some(Duration::from_secs(3)),
            "source-specific override should win"
        );

        assert_eq!(
            shaping_config
                .connector_client_config("explicit_null")
                .pool_idle_timeout,
            Some(Duration::from_secs(20)),
            "explicit null falls back to all"
        );

        assert_eq!(
            shaping_config
                .connector_client_config("unknown")
                .pool_idle_timeout,
            Some(Duration::from_secs(20)),
            "unknown source falls back to all"
        );
    }

    #[tokio::test]
    async fn test_pool_idle_timeout_uses_default_when_not_configured() {
        let config = serde_yaml::from_str::<Config>(
            r#"
        all:
          http2: disable
        router:
          timeout: 65s
        "#,
        )
        .unwrap();

        let shaping_config = TrafficShaping::new(PluginInit::fake_builder().config(config).build())
            .await
            .unwrap();

        assert_eq!(
            shaping_config
                .subgraph_client_config("any")
                .pool_idle_timeout,
            default_pool_idle_timeout(),
            "when pool_idle_timeout is not in the config, it should use the default"
        );
    }

    #[tokio::test]
    async fn test_subgraph_keep_alive_override_and_fallback() {
        let config = serde_yaml::from_str::<Config>(
            r#"
        all:
          experimental_http2_keep_alive_interval: 30s
          experimental_http2_keep_alive_timeout: 10s
        subgraphs:
          fast:
            experimental_http2_keep_alive_interval: 5s
          explicit_null:
            experimental_http2_keep_alive_interval: null
        "#,
        )
        .unwrap();

        let shaping_config = TrafficShaping::new(PluginInit::fake_builder().config(config).build())
            .await
            .unwrap();

        assert_eq!(
            shaping_config
                .subgraph_client_config("fast")
                .experimental_http2_keep_alive_interval,
            Some(Duration::from_secs(5)),
            "subgraph-specific override should win"
        );

        assert_eq!(
            shaping_config
                .subgraph_client_config("explicit_null")
                .experimental_http2_keep_alive_interval,
            Some(Duration::from_secs(30)),
            "explicit null falls back to all"
        );

        assert_eq!(
            shaping_config
                .subgraph_client_config("unknown")
                .experimental_http2_keep_alive_interval,
            Some(Duration::from_secs(30)),
            "unknown subgraph falls back to all"
        );

        assert_eq!(
            shaping_config
                .subgraph_client_config("unknown")
                .experimental_http2_keep_alive_timeout,
            Some(Duration::from_secs(10)),
            "timeout is inherited from all"
        );
    }

    #[tokio::test]
    async fn test_connector_keep_alive_override_and_fallback() {
        let config = serde_yaml::from_str::<Config>(
            r#"
        connector:
          all:
            experimental_http2_keep_alive_interval: 30s
            experimental_http2_keep_alive_timeout: 10s
          sources:
            my_source:
              experimental_http2_keep_alive_interval: 5s
            explicit_null:
              experimental_http2_keep_alive_interval: null
        "#,
        )
        .unwrap();

        let shaping_config = TrafficShaping::new(PluginInit::fake_builder().config(config).build())
            .await
            .unwrap();

        assert_eq!(
            shaping_config
                .connector_client_config("my_source")
                .experimental_http2_keep_alive_interval,
            Some(Duration::from_secs(5)),
            "source-specific override should win"
        );

        assert_eq!(
            shaping_config
                .connector_client_config("explicit_null")
                .experimental_http2_keep_alive_interval,
            Some(Duration::from_secs(30)),
            "explicit null falls back to all"
        );

        assert_eq!(
            shaping_config
                .connector_client_config("unknown")
                .experimental_http2_keep_alive_interval,
            Some(Duration::from_secs(30)),
            "unknown source falls back to all"
        );

        assert_eq!(
            shaping_config
                .connector_client_config("unknown")
                .experimental_http2_keep_alive_timeout,
            Some(Duration::from_secs(10)),
            "timeout is inherited from all"
        );
    }

    #[tokio::test]
    async fn test_keep_alive_is_none_when_not_configured() {
        let config = serde_yaml::from_str::<Config>("{}").unwrap();

        let shaping_config = TrafficShaping::new(PluginInit::fake_builder().config(config).build())
            .await
            .unwrap();

        assert_eq!(
            shaping_config
                .subgraph_client_config("any")
                .experimental_http2_keep_alive_interval,
            None,
            "keep-alive interval should be None when not configured"
        );
        assert_eq!(
            shaping_config
                .subgraph_client_config("any")
                .experimental_http2_keep_alive_timeout,
            None,
            "keep-alive timeout should be None when not configured"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_raises_different_errors_for_timeouts_and_rate_limits() {
        // expected behavior: the first request sent will timeout, the second request will return
        // immediately due to the ratelimit
        let config: serde_json::Value = serde_yaml::from_str(
            r#"
        router:
            global_rate_limit:
                capacity: 1
                interval: 100ms
            timeout: 150ms
        "#,
        )
        .unwrap();

        let plugin = get_traffic_shaping_plugin(&config).await;
        let svc = ServiceBuilder::new()
            .service_fn(move |_req: router::Request| async {
                sleep(Duration::from_millis(500)).await;
                RouterResponse::fake_builder().build()
            })
            .boxed_clone();

        let mut router_service = plugin.router_service(svc);

        let mut tasks = JoinSet::new();
        for _ in 0..2 {
            let request = RouterRequest::fake_builder().build().unwrap();
            tasks.spawn(router_service.ready().await.unwrap().call(request));
        }

        let mut results = tasks.join_all().await.into_iter();

        let response = results.next().unwrap().unwrap().response;
        assert_eq!(StatusCode::SERVICE_UNAVAILABLE, response.status());

        let response = results.next().unwrap().unwrap().response;
        assert_eq!(StatusCode::GATEWAY_TIMEOUT, response.status());
    }
}
