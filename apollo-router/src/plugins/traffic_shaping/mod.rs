//! Traffic shaping plugin
//!
//! Currently includes:
//! * Query deduplication
//! * Timeout
//! * Compression
//! * Rate limiting
//!

mod deduplication;
mod rate;
mod retry;
mod timeout;

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::Mutex;
use std::time::Duration;

use futures::future::BoxFuture;
use http::header::ACCEPT_ENCODING;
use http::header::CONTENT_ENCODING;
use http::HeaderValue;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::retry::Retry;
use tower::util::Either;
use tower::util::Oneshot;
use tower::BoxError;
use tower::Service;
use tower::ServiceBuilder;
use tower::ServiceExt;

use self::deduplication::QueryDeduplicationLayer;
use self::rate::RateLimitLayer;
pub(crate) use self::rate::RateLimited;
use self::retry::RetryPolicy;
pub(crate) use self::timeout::Elapsed;
use self::timeout::TimeoutLayer;
use crate::error::ConfigurationError;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::subgraph;
use crate::services::subgraph_service::Compression;
use crate::services::supergraph;
use crate::services::SubgraphRequest;
use crate::Configuration;

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
    /// Enable APQ for outgoing subgraph requests
    apq: Option<bool>,
    /// Retry configuration
    //  *experimental feature*: Enables request retry
    experimental_retry: Option<RetryConfig>,
}

impl Merge for Shaping {
    fn merge(&self, fallback: Option<&Self>) -> Self {
        match fallback {
            None => self.clone(),
            Some(fallback) => Shaping {
                deduplicate_query: self.deduplicate_query.or(fallback.deduplicate_query),
                compression: self.compression.or(fallback.compression),
                timeout: self.timeout.or(fallback.timeout),
                apq: self.apq.or(fallback.apq),
                global_rate_limit: self
                    .global_rate_limit
                    .as_ref()
                    .or(fallback.global_rate_limit.as_ref())
                    .cloned(),
                experimental_retry: self
                    .experimental_retry
                    .as_ref()
                    .or(fallback.experimental_retry.as_ref())
                    .cloned(),
            },
        }
    }
}

/// Retry configuration
#[derive(PartialEq, Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RetryConfig {
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "String", default)]
    /// how long a single deposit should be considered. Must be between 1 and 60 seconds,
    /// default value is 10 seconds
    ttl: Option<Duration>,
    /// minimum rate of retries allowed to accomodate clients that have just started
    /// issuing requests, or clients that do not issue many requests per window. The
    /// default value is 10
    min_per_sec: Option<u32>,
    /// percentage of calls to deposit that can be retried. This is in addition to any
    /// retries allowed for via min_per_sec. Must be between 0 and 1000, default value
    /// is 0.2
    retry_percent: Option<f32>,
    /// allows request retries on mutations. This should only be activated if mutations
    /// are idempotent. Disabled by default
    retry_mutations: Option<bool>,
}

impl Merge for RetryConfig {
    fn merge(&self, fallback: Option<&Self>) -> Self {
        match fallback {
            None => self.clone(),
            Some(fallback) => RetryConfig {
                ttl: self.ttl.or(fallback.ttl),
                min_per_sec: self.min_per_sec.or(fallback.min_per_sec),
                retry_percent: self.retry_percent.or(fallback.retry_percent),
                retry_mutations: self.retry_mutations.or(fallback.retry_mutations),
            },
        }
    }
}

#[derive(PartialEq, Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RouterShaping {
    /// Enable global rate limiting
    global_rate_limit: Option<RateLimitConf>,
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "String", default)]
    /// Enable timeout for incoming requests
    timeout: Option<Duration>,
}

#[derive(PartialEq, Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
// FIXME: This struct is pub(crate) because we need its configuration in the query planner service.
// Remove this once the configuration yml changes.
/// Configuration for the experimental traffic shaping plugin
pub(crate) struct Config {
    #[serde(default)]
    /// Applied at the router level
    router: Option<RouterShaping>,
    #[serde(default)]
    /// Applied on all subgraphs
    all: Option<Shaping>,
    #[serde(default)]
    /// Applied on specific subgraphs
    subgraphs: HashMap<String, Shaping>,
    /// Enable variable deduplication optimization when sending requests to subgraphs (https://github.com/apollographql/router/issues/87)
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
    rate_limit_router: Option<RateLimitLayer>,
    rate_limit_subgraphs: Mutex<HashMap<String, RateLimitLayer>>,
}

#[async_trait::async_trait]
impl Plugin for TrafficShaping {
    type Config = Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let rate_limit_router = init
            .config
            .router
            .as_ref()
            .and_then(|r| r.global_rate_limit.as_ref())
            .map(|router_rate_limit_conf| {
                if router_rate_limit_conf.interval.as_millis() > u64::MAX as u128 {
                    Err(ConfigurationError::InvalidConfiguration {
                        message: "bad configuration for traffic_shaping plugin",
                        error: format!(
                            "cannot set an interval for the rate limit greater than {} ms",
                            u64::MAX
                        ),
                    })
                } else {
                    Ok(RateLimitLayer::new(
                        router_rate_limit_conf.capacity,
                        router_rate_limit_conf.interval,
                    ))
                }
            })
            .transpose()?;

        Ok(Self {
            config: init.config,
            rate_limit_router,
            rate_limit_subgraphs: Mutex::new(HashMap::new()),
        })
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

    pub(crate) fn supergraph_service_internal<S>(
        &self,
        service: S,
    ) -> impl Service<
        supergraph::Request,
        Response = supergraph::Response,
        Error = BoxError,
        Future = timeout::future::ResponseFuture<
            Oneshot<tower::util::Either<rate::service::RateLimit<S>, S>, supergraph::Request>,
        >,
    > + Clone
           + Send
           + Sync
           + 'static
    where
        S: Service<supergraph::Request, Response = supergraph::Response, Error = BoxError>
            + Clone
            + Send
            + Sync
            + 'static,
        <S as Service<supergraph::Request>>::Future: std::marker::Send,
    {
        ServiceBuilder::new()
            .layer(TimeoutLayer::new(
                self.config
                    .router
                    .as_ref()
                    .and_then(|r| r.timeout)
                    .unwrap_or(DEFAULT_TIMEOUT),
            ))
            .option_layer(self.rate_limit_router.clone())
            .service(service)
    }

    pub(crate) fn subgraph_service_internal<S>(
        &self,
        name: &str,
        service: S,
    ) -> impl Service<
        subgraph::Request,
        Response = subgraph::Response,
        Error = BoxError,
        Future = tower::util::Either<
            tower::util::Either<
                BoxFuture<'static, Result<subgraph::Response, BoxError>>,
                timeout::future::ResponseFuture<
                    Oneshot<
                        tower::util::Either<
                            Retry<RetryPolicy, tower::util::Either<rate::service::RateLimit<S>, S>>,
                            tower::util::Either<rate::service::RateLimit<S>, S>,
                        >,
                        subgraph::Request,
                    >,
                >,
            >,
            <S as Service<subgraph::Request>>::Future,
        >,
    > + Clone
           + Send
           + Sync
           + 'static
    where
        S: Service<subgraph::Request, Response = subgraph::Response, Error = BoxError>
            + Clone
            + Send
            + Sync
            + 'static,
        <S as Service<subgraph::Request>>::Future: std::marker::Send,
    {
        // Either we have the subgraph config and we merge it with the all config, or we just have the all config or we have nothing.
        let all_config = self.config.all.as_ref();
        let subgraph_config = self.config.subgraphs.get(name);
        let final_config = Self::merge_config(all_config, subgraph_config);

        if let Some(config) = final_config {
            let rate_limit = config.global_rate_limit.as_ref().map(|rate_limit_conf| {
                self.rate_limit_subgraphs
                    .lock()
                    .unwrap()
                    .entry(name.to_string())
                    .or_insert_with(|| {
                        RateLimitLayer::new(rate_limit_conf.capacity, rate_limit_conf.interval)
                    })
                    .clone()
            });

            let retry = config.experimental_retry.as_ref().map(|config| {
                let retry_policy = RetryPolicy::new(
                    config.ttl,
                    config.min_per_sec,
                    config.retry_percent,
                    config.retry_mutations,
                );
                tower::retry::RetryLayer::new(retry_policy)
            });

            Either::A(ServiceBuilder::new()
                .option_layer(config.deduplicate_query.unwrap_or_default().then(
                  QueryDeduplicationLayer::default
                ))
                    .layer(TimeoutLayer::new(
                        config
                        .timeout
                        .unwrap_or(DEFAULT_TIMEOUT),
                    ))
                    .option_layer(retry)
                    .option_layer(rate_limit)
                .service(service)
                .map_request(move |mut req: SubgraphRequest| {
                    if let Some(compression) = config.compression {
                        let compression_header_val = HeaderValue::from_str(&compression.to_string()).expect("compression is manually implemented and already have the right values; qed");
                        req.subgraph_request.headers_mut().insert(ACCEPT_ENCODING, HeaderValue::from_static("gzip, br, deflate"));
                        req.subgraph_request.headers_mut().insert(CONTENT_ENCODING, compression_header_val);
                    }

                    req
                }))
        } else {
            Either::B(service)
        }
    }

    pub(crate) fn get_apq(&self, name: &str) -> Option<bool> {
        self.config.subgraphs.get(name)?.apq
    }
}

impl TrafficShaping {
    pub(crate) fn get_configuration_deduplicate_variables(configuration: &Configuration) -> bool {
        configuration
            .plugin_configuration(APOLLO_TRAFFIC_SHAPING)
            .map(|conf| conf.get("deduplicate_variables") == Some(&serde_json::Value::Bool(true)))
            .unwrap_or_default()
    }
}

register_plugin!("apollo", "traffic_shaping", TrafficShaping);

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use bytes::Bytes;
    use once_cell::sync::Lazy;
    use serde_json_bytes::json;
    use serde_json_bytes::ByteString;
    use serde_json_bytes::Value;
    use tower::Service;

    use super::*;
    use crate::json_ext::Object;
    use crate::plugin::test::MockSubgraph;
    use crate::plugin::test::MockSupergraphService;
    use crate::plugin::DynPlugin;
    use crate::router_factory::create_plugins;
    use crate::services::router;
    use crate::services::router_service::RouterCreator;
    use crate::services::PluggableSupergraphServiceBuilder;
    use crate::services::SupergraphRequest;
    use crate::services::SupergraphResponse;
    use crate::spec::Schema;
    use crate::Configuration;

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
        let schema: Arc<Schema> = Arc::new(Schema::parse(schema, &Default::default()).unwrap());

        let config: Configuration = serde_yaml::from_str(
            r#"
        traffic_shaping:
            deduplicate_variables: true
        "#,
        )
        .unwrap();

        let config = Arc::new(config);

        let mut builder = PluggableSupergraphServiceBuilder::new(schema.clone())
            .with_configuration(config.clone());

        for (name, plugin) in create_plugins(
            &config,
            &schema,
            Some(vec![(APOLLO_TRAFFIC_SHAPING.to_string(), plugin)]),
        )
        .await
        .expect("create plugins should work")
        .into_iter()
        {
            builder = builder.with_dyn_plugin(name, plugin);
        }

        let builder = builder
            .with_subgraph_service("accounts", account_service.clone())
            .with_subgraph_service("reviews", review_service.clone())
            .with_subgraph_service("products", product_service.clone());

        RouterCreator::new(
            Arc::new(builder.build().await.expect("should build")),
            &Configuration::default(),
        )
        .await
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
            assert_eq!(
                req.subgraph_request
                    .headers()
                    .get(&ACCEPT_ENCODING)
                    .unwrap(),
                HeaderValue::from_static("gzip, br, deflate")
            );

            req
        });

        let _response = plugin
            .as_any()
            .downcast_ref::<TrafficShaping>()
            .unwrap()
            .subgraph_service_internal("test", test_service)
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

        let test_service = MockSubgraph::new(HashMap::new());

        let _response = plugin
            .as_any()
            .downcast_ref::<TrafficShaping>()
            .unwrap()
            .subgraph_service_internal("test", test_service.clone())
            .oneshot(SubgraphRequest::fake_builder().build())
            .await
            .unwrap();
        let _response = plugin
            .as_any()
            .downcast_ref::<TrafficShaping>()
            .unwrap()
            .subgraph_service_internal("test", test_service.clone())
            .oneshot(SubgraphRequest::fake_builder().build())
            .await
            .expect_err("should be in error due to a timeout and rate limit");
        let _response = plugin
            .as_any()
            .downcast_ref::<TrafficShaping>()
            .unwrap()
            .subgraph_service_internal("another", test_service.clone())
            .oneshot(SubgraphRequest::fake_builder().build())
            .await
            .unwrap();
        // Note: use `timeout` to guarantee 300ms has elapsed
        let big_sleep = tokio::time::sleep(Duration::from_secs(10));
        assert!(tokio::time::timeout(Duration::from_millis(300), big_sleep)
            .await
            .is_err());
        let _response = plugin
            .as_any()
            .downcast_ref::<TrafficShaping>()
            .unwrap()
            .subgraph_service_internal("test", test_service.clone())
            .oneshot(SubgraphRequest::fake_builder().build())
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn it_rate_limit_router_requests() {
        let config = serde_yaml::from_str::<serde_json::Value>(
            r#"
        router:
            global_rate_limit:
                capacity: 1
                interval: 300ms
            timeout: 500ms
        "#,
        )
        .unwrap();

        let plugin = get_traffic_shaping_plugin(&config).await;
        let mut mock_service = MockSupergraphService::new();
        mock_service.expect_clone().returning(|| {
            let mut mock_service = MockSupergraphService::new();

            mock_service.expect_clone().returning(|| {
                let mut mock_service = MockSupergraphService::new();
                mock_service.expect_call().times(0..2).returning(move |_| {
                    Ok(SupergraphResponse::fake_builder()
                        .data(json!({ "test": 1234_u32 }))
                        .build()
                        .unwrap())
                });
                mock_service
            });
            mock_service
        });

        let _response = plugin
            .as_any()
            .downcast_ref::<TrafficShaping>()
            .unwrap()
            .supergraph_service_internal(mock_service.clone())
            .oneshot(SupergraphRequest::fake_builder().build().unwrap())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();

        assert!(plugin
            .as_any()
            .downcast_ref::<TrafficShaping>()
            .unwrap()
            .supergraph_service_internal(mock_service.clone())
            .oneshot(SupergraphRequest::fake_builder().build().unwrap())
            .await
            .is_err());
        // Note: use `timeout` to guarantee 300ms has elapsed
        let big_sleep = tokio::time::sleep(Duration::from_secs(10));
        assert!(tokio::time::timeout(Duration::from_millis(300), big_sleep)
            .await
            .is_err());
        let _response = plugin
            .as_any()
            .downcast_ref::<TrafficShaping>()
            .unwrap()
            .supergraph_service_internal(mock_service.clone())
            .oneshot(SupergraphRequest::fake_builder().build().unwrap())
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();
    }
}
