//! Health Check plugin
//!
//! Provides liveness and readiness checks for the router.
//!

use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use http::StatusCode;
use multimap::MultiMap;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tower::service_fn;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::configuration::ListenAddr;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::register_private_plugin;
use crate::services::router;
use crate::Endpoint;

#[derive(Debug, Serialize)]
#[serde(rename_all = "UPPERCASE")]
#[allow(dead_code)]
enum HealthStatus {
    Up,
    Down,
}

#[derive(Debug, Serialize)]
struct Health {
    status: HealthStatus,
}

/// Configuration options pertaining to the http server component.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub(crate) struct Config {
    /// The socket address and port to listen on
    /// Defaults to 127.0.0.1:8088
    pub(crate) listen: ListenAddr,

    /// Set to false to disable the health check
    pub(crate) enabled: bool,

    /// Optionally set a custom healthcheck path
    /// Defaults to /health
    pub(crate) path: String,
}

#[cfg(test)]
pub(crate) fn test_listen() -> ListenAddr {
    SocketAddr::from_str("127.0.0.1:0").unwrap().into()
}

fn default_health_check_listen() -> ListenAddr {
    SocketAddr::from_str("127.0.0.1:8088").unwrap().into()
}

fn default_health_check_enabled() -> bool {
    true
}

fn default_health_check_path() -> String {
    "/health".to_string()
}

#[buildstructor::buildstructor]
impl Config {
    #[builder]
    pub(crate) fn new(
        listen: Option<ListenAddr>,
        enabled: Option<bool>,
        path: Option<String>,
    ) -> Self {
        let mut path = path.unwrap_or_else(default_health_check_path);
        if !path.starts_with('/') {
            path = format!("/{path}").to_string();
        }

        Self {
            listen: listen.unwrap_or_else(default_health_check_listen),
            enabled: enabled.unwrap_or_else(default_health_check_enabled),
            path,
        }
    }
}

#[cfg(test)]
#[buildstructor::buildstructor]
impl Config {
    #[builder]
    pub(crate) fn fake_new(
        listen: Option<ListenAddr>,
        enabled: Option<bool>,
        path: Option<String>,
    ) -> Self {
        let mut path = path.unwrap_or_else(default_health_check_path);
        if !path.starts_with('/') {
            path = format!("/{path}");
        }

        Self {
            listen: listen.unwrap_or_else(test_listen),
            enabled: enabled.unwrap_or_else(default_health_check_enabled),
            path,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::builder().build()
    }
}

struct HealthCheck {
    config: Config,
    live: Arc<AtomicBool>,
    ready: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl PluginPrivate for HealthCheck {
    type Config = Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        tracing::info!(
            "Health check exposed at {}{}",
            init.config.listen,
            init.config.path
        );
        Ok(Self {
            config: init.config,
            live: Arc::new(AtomicBool::new(false)),
            ready: Arc::new(AtomicBool::new(true)),
        })
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        ServiceBuilder::new()
            .map_response(|response: router::Response| {
                println!("response: {response:?}");
                response
            })
            .service(service)
            .boxed()
    }

    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
        let mut map = MultiMap::new();

        let my_ready = self.ready.clone();
        let my_live = self.live.clone();

        let endpoint = Endpoint::from_router_service(
            self.config.path.clone(),
            service_fn(move |req: router::Request| {
                let mut status_code = StatusCode::OK;
                let health = if let Some(query) = req.router_request.uri().query() {
                    let query_upper = query.to_ascii_uppercase();
                    // Could be more precise, but sloppy match is fine for this use case
                    if query_upper.starts_with("READY") {
                        let status = if my_ready.load(Ordering::SeqCst) {
                            HealthStatus::Up
                        } else {
                            // It's hard to get k8s to parse payloads. Especially since we
                            // can't install curl or jq into our docker images because of CVEs.
                            // So, compromise, k8s will interpret this as probe fail.
                            status_code = StatusCode::SERVICE_UNAVAILABLE;
                            HealthStatus::Down
                        };
                        Health { status }
                    } else if query_upper.starts_with("LIVE") {
                        let status = if my_live.load(Ordering::SeqCst) {
                            HealthStatus::Up
                        } else {
                            // It's hard to get k8s to parse payloads. Especially since we
                            // can't install curl or jq into our docker images because of CVEs.
                            // So, compromise, k8s will interpret this as probe fail.
                            status_code = StatusCode::SERVICE_UNAVAILABLE;
                            HealthStatus::Down
                        };
                        Health { status }
                    } else {
                        Health {
                            status: HealthStatus::Up,
                        }
                    }
                } else {
                    Health {
                        status: HealthStatus::Up,
                    }
                };
                tracing::trace!(?health, request = ?req.router_request, "health check");
                async move {
                    Ok(router::Response {
                        response: http::Response::builder().status(status_code).body(
                            router::body::from_bytes(
                                serde_json::to_vec(&health).map_err(BoxError::from)?,
                            ),
                        )?,
                        context: req.context,
                    })
                }
            })
            .boxed(),
        );

        println!("ADDING A HEALTH LISTEN AT: {:?}", self.config.listen);
        map.insert(self.config.listen.clone(), endpoint);

        map
    }

    /// The point of no return this plugin is about to go live
    fn activate(&self) {
        self.live.store(true, Ordering::SeqCst);
    }
}

register_private_plugin!("apollo", "healthcheck", HealthCheck);

/*
#[cfg(test)]
mod test {
    use std::sync::Arc;

    use bytes::Bytes;
    use maplit::hashmap;
    use once_cell::sync::Lazy;
    use serde_json_bytes::json;
    use serde_json_bytes::ByteString;
    use serde_json_bytes::Value;
    use tower::Service;

    use super::*;
    use crate::json_ext::Object;
    use crate::plugin::test::MockRouterService;
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
    use crate::services::RouterRequest;
    use crate::services::RouterResponse;
    use crate::services::SupergraphRequest;
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

        assert!(svc
            .ready()
            .await
            .expect("it is ready")
            .call(SubgraphRequest::fake_builder().build())
            .await
            .unwrap()
            .response
            .body()
            .errors
            .is_empty());
        let response = svc
            .ready()
            .await
            .expect("it is ready")
            .call(SubgraphRequest::fake_builder().build())
            .await
            .expect("it responded");

        assert_eq!(StatusCode::SERVICE_UNAVAILABLE, response.response.status());

        tokio::time::sleep(Duration::from_millis(300)).await;

        assert!(svc
            .ready()
            .await
            .expect("it is ready")
            .call(SubgraphRequest::fake_builder().build())
            .await
            .unwrap()
            .response
            .body()
            .errors
            .is_empty());
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
            &crate::services::router::body::into_bytes(response.response)
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
*/
