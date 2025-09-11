use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use apollo_router::graphql;
use apollo_router::services;
use apollo_router::test_harness::HttpService;
use http::HeaderMap;
use http_body_util::BodyExt as _;
use indexmap::IndexMap;
use serde_json::json;
use tower::Service as _;
use tower::ServiceExt as _;

use crate::integration::common::IntegrationTest;
use crate::integration::common::Query;
use crate::integration::common::graph_os_enabled;

const INVALIDATION_PATH: &str = "/invalidation";
const INVALIDATION_SHARED_KEY: &str = "supersecret";

fn base_config() -> serde_json::Value {
    // Isolate tests from each other by adding a random redis key prefix
    let namespace = uuid::Uuid::new_v4().simple().to_string();
    json!({
        "include_subgraph_errors": {
            "all": true,
        },
        "preview_entity_cache": {
            "enabled": true,
            "subgraph": {
                "all": {
                    "redis": {
                        "urls": ["redis://127.0.0.1:6379"],
                        "ttl": "10m",
                        "namespace": namespace,
                        "required_to_start": true,
                    },
                    "invalidation": {
                        "enabled": true,
                        "shared_key": INVALIDATION_SHARED_KEY,
                    },
                },
            },
            "invalidation": {
                "listen": "127.0.0.1:4000",
                "path": INVALIDATION_PATH,
            },
        },
    })
}

fn base_subgraphs() -> serde_json::Value {
    json!({
        "products": {
            "headers": {"cache-control": "public"},
            "query": {
                "topProducts": [
                    {"upc": "1"},
                    {"upc": "2"},
                ],
            },
        },
        "reviews": {
            "headers": {"cache-control": "public"},
            "entities": [
                {"__typename": "Product", "upc": "1", "reviews": [{"id": "r1a"}, {"id": "r1b"}]},
                {"__typename": "Product", "upc": "2", "reviews": [{"id": "r2"}]},
            ],
        },
    })
}

async fn harness(
    mut config: serde_json::Value,
    subgraphs: serde_json::Value,
) -> (HttpService, Arc<IndexMap<String, Arc<AtomicUsize>>>) {
    let counters = Arc::new(IndexMap::from([
        ("products".into(), Default::default()),
        ("reviews".into(), Default::default()),
    ]));
    let counters2 = Arc::clone(&counters);
    config
        .as_object_mut()
        .unwrap()
        .insert("experimental_mock_subgraphs".into(), subgraphs);
    let router = apollo_router::TestHarness::builder()
        .schema(include_str!("../../testing_schema.graphql"))
        .configuration_json(config)
        .unwrap()
        .subgraph_hook(move |subgraph_name, service| {
            if let Some(counter) = counters2.get(subgraph_name) {
                let counter = Arc::<AtomicUsize>::clone(counter);
                service
                    .map_request(move |req| {
                        counter.fetch_add(1, Ordering::Relaxed);
                        req
                    })
                    .boxed()
            } else {
                service
            }
        })
        .build_http_service()
        .await
        .unwrap();
    (router, counters)
}

async fn make_graphql_request(router: &mut HttpService) -> (HeaderMap<String>, graphql::Response) {
    let query = "{ topProducts { reviews { id } } }";
    let request = graphql_request(query);
    make_http_request(router, request.into()).await
}

fn graphql_request(query: &str) -> services::router::Request {
    services::supergraph::Request::fake_builder()
        .query(query)
        .build()
        .unwrap()
        .try_into()
        .unwrap()
}

async fn make_json_request(
    router: &mut HttpService,
    request: http::Request<serde_json::Value>,
) -> (HeaderMap<String>, serde_json::Value) {
    let request =
        request.map(|body| services::router::body::from_bytes(serde_json::to_vec(&body).unwrap()));
    make_http_request(router, request).await
}

async fn make_http_request<ResponseBody>(
    router: &mut HttpService,
    request: http::Request<apollo_router::services::router::Body>,
) -> (HeaderMap<String>, ResponseBody)
where
    ResponseBody: for<'a> serde::Deserialize<'a>,
{
    let response = router.ready().await.unwrap().call(request).await.unwrap();
    let headers = response
        .headers()
        .iter()
        .map(|(k, v)| (k.clone(), v.to_str().unwrap().to_owned()))
        .collect();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (headers, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn basic_cache_skips_subgraph_request() {
    if !graph_os_enabled() {
        return;
    }

    let (mut router, subgraph_request_counters) = harness(base_config(), base_subgraphs()).await;
    insta::assert_yaml_snapshot!(subgraph_request_counters, @r###"
        products: 0
        reviews: 0
    "###);
    let (headers, body) = make_graphql_request(&mut router).await;
    assert!(headers["cache-control"].contains("public"));
    insta::assert_yaml_snapshot!(body, @r###"
        data:
          topProducts:
            - reviews:
                - id: r1a
                - id: r1b
            - reviews:
                - id: r2
    "###);
    insta::assert_yaml_snapshot!(subgraph_request_counters, @r###"
        products: 1
        reviews: 1
    "###);
    let (headers, body) = make_graphql_request(&mut router).await;
    assert!(headers["cache-control"].contains("public"));
    insta::assert_yaml_snapshot!(body, @r###"
        data:
          topProducts:
            - reviews:
                - id: r1a
                - id: r1b
            - reviews:
                - id: r2
    "###);
    // Unchanged, everything is in cache so we don’t need to make more subgraph requests:
    insta::assert_yaml_snapshot!(subgraph_request_counters, @r###"
        products: 1
        reviews: 1
    "###);
}

#[tokio::test]
async fn not_cached_without_cache_control_header() {
    if !graph_os_enabled() {
        return;
    }

    let mut subgraphs = base_subgraphs();
    subgraphs["products"]
        .as_object_mut()
        .unwrap()
        .remove("headers");
    subgraphs["reviews"]
        .as_object_mut()
        .unwrap()
        .remove("headers");
    let (mut router, subgraph_request_counters) = harness(base_config(), subgraphs).await;
    insta::assert_yaml_snapshot!(subgraph_request_counters, @r###"
        products: 0
        reviews: 0
    "###);
    let (headers, body) = make_graphql_request(&mut router).await;
    // When subgraphs don’t set a cache-control header, Router defaults to not caching
    // and instructs any downstream cache to do the same:
    assert_eq!(headers["cache-control"], "no-store");
    insta::assert_yaml_snapshot!(body, @r###"
        data:
          topProducts:
            - reviews:
                - id: r1a
                - id: r1b
            - reviews:
                - id: r2
    "###);
    insta::assert_yaml_snapshot!(subgraph_request_counters, @r###"
        products: 1
        reviews: 1
    "###);
    let (headers, body) = make_graphql_request(&mut router).await;
    assert_eq!(headers["cache-control"], "no-store");
    insta::assert_yaml_snapshot!(body, @r###"
        data:
          topProducts:
            - reviews:
                - id: r1a
                - id: r1b
            - reviews:
                - id: r2
    "###);
    // More supergraph requsets lead to more subgraph requests:
    insta::assert_yaml_snapshot!(subgraph_request_counters, @r###"
        products: 2
        reviews: 2
    "###);
}

#[tokio::test]
async fn invalidate_with_endpoint() {
    if !graph_os_enabled() {
        return;
    }

    let (mut router, subgraph_request_counters) = harness(base_config(), base_subgraphs()).await;
    let (headers, body) = make_graphql_request(&mut router).await;
    assert!(headers["cache-control"].contains("public"));
    assert!(body.errors.is_empty());
    insta::assert_yaml_snapshot!(subgraph_request_counters, @r###"
        products: 1
        reviews: 1
    "###);

    let request = http::Request::builder()
        .method("POST")
        .uri(INVALIDATION_PATH)
        .header("Authorization", INVALIDATION_SHARED_KEY)
        .body(json!([{
            "kind": "entity",
            "subgraph": "reviews",
            "type": "Product",
            "key": {
                "upc": "1",
            },
        }]))
        .unwrap();
    let (_headers, body) = make_json_request(&mut router, request).await;
    insta::assert_yaml_snapshot!(body, @"count: 1");

    let (headers, body) = make_graphql_request(&mut router).await;
    assert!(headers["cache-control"].contains("public"));
    assert!(body.errors.is_empty());
    // After invalidation, reviews need to be requested again but products are still in cache:
    insta::assert_yaml_snapshot!(subgraph_request_counters, @r###"
        products: 1
        reviews: 2
    "###);
}

#[tokio::test]
async fn cache_control_merging_single_fetch() {
    if !graph_os_enabled() {
        return;
    }

    let mut subgraphs = base_subgraphs();
    subgraphs["products"]["headers"]["cache-control"] = "public, s-maxage=120".into();
    subgraphs["reviews"]["headers"]["cache-control"] = "public, s-maxage=60".into();
    let (mut router, _subgraph_request_counters) = harness(base_config(), subgraphs).await;
    let query = "{ topProducts { upc } }";

    // Router responds with `max-age` even if a single subgraph used `s-maxage`
    let (headers, _body) =
        make_http_request::<graphql::Response>(&mut router, graphql_request(query).into()).await;
    insta::assert_snapshot!(&headers["cache-control"], @"max-age=120,public");

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let query = "{ topProducts { upc } }";
    let (headers, _body) =
        make_http_request::<graphql::Response>(&mut router, graphql_request(query).into()).await;
    let cache_control = &headers["cache-control"];
    let max_age = parse_max_age(cache_control);
    // Usually 120 - 2 = 118, but allow some slack in case CI CPUs are busy
    assert!(max_age > 100 && max_age < 120, "got '{cache_control}'");
}

#[tokio::test]
async fn cache_control_merging_multi_fetch() {
    if !graph_os_enabled() {
        return;
    }

    let mut subgraphs = base_subgraphs();
    subgraphs["products"]["headers"]["cache-control"] = "public, s-maxage=120".into();
    subgraphs["reviews"]["headers"]["cache-control"] = "public, s-maxage=60".into();
    let (mut router, _subgraph_request_counters) = harness(base_config(), subgraphs).await;
    let query = "{ topProducts { reviews { id } } }";

    // Router responds with `max-age` even if a subgraphs used `s-maxage`.
    // The smaller value is used.
    let (headers, _body) =
        make_http_request::<graphql::Response>(&mut router, graphql_request(query).into()).await;
    insta::assert_snapshot!(&headers["cache-control"], @"max-age=60,public");

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let (headers, _body) =
        make_http_request::<graphql::Response>(&mut router, graphql_request(query).into()).await;
    let cache_control = &headers["cache-control"];
    let max_age = parse_max_age(cache_control);
    // Usually 60 - 2 = 58, but allow some slack in case CI CPUs are busy
    assert!(max_age > 40 && max_age < 60, "got '{cache_control}'");
}

fn parse_max_age(cache_control: &str) -> u32 {
    cache_control
        .strip_prefix("max-age=")
        .and_then(|s| s.strip_suffix(",public"))
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("expected 'max-age={{seconds}},public', got '{cache_control}'"))
}

fn subgraphs_with_many_entities(count: usize) -> serde_json::Value {
    let mut reviews = vec![];
    let mut top_products = vec![];
    for upc in 1..=count {
        top_products.push(json!({ "upc": upc.to_string() }));
        reviews.push(json!({
            "__typename": "Product",
            "upc": upc.to_string(),
            "reviews": [{ "id": format!("r{upc}") }],
        }));
    }

    json!({
        "products": {
            "headers": {"cache-control": "public"},
            "query": { "topProducts": top_products },
        },
        "reviews": {
            "headers": {"cache-control": "public"},
            "entities": reviews,
        },
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn test_cache_metrics() {
    if !graph_os_enabled() {
        return;
    }

    const NUM_PRODUCTS: usize = 1_000;

    // Create configuration with Redis cache and prometheus metrics
    let namespace = uuid::Uuid::new_v4().simple().to_string();
    let config = json!({
        "include_subgraph_errors": {
            "all": true,
        },
        "preview_entity_cache": {
            "enabled": true,
            "subgraph": {
                "all": {
                    "redis": {
                        "urls": ["redis://127.0.0.1:6379"],
                        "ttl": "10m",
                        "namespace": namespace,
                        "required_to_start": true,
                        "metrics_interval": "100ms",
                    },
                    "invalidation": {
                        "enabled": true,
                        "shared_key": INVALIDATION_SHARED_KEY,
                    },
                },
            },
            "invalidation": {
                "listen": "127.0.0.1:4000",
                "path": INVALIDATION_PATH,
            },
        },
        "telemetry": {
            "exporters": {
                "metrics": {
                    "prometheus": {
                        "enabled": true,
                        "listen": "127.0.0.1:0",
                        "path": "/metrics",
                    },
                },
            },
        },
        "experimental_mock_subgraphs": subgraphs_with_many_entities(NUM_PRODUCTS),
    });

    let mut router = IntegrationTest::builder()
        .config(serde_yaml::to_string(&config).unwrap())
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Execute the first query - this should populate the cache
    let query = Query::builder()
        .body(json!({"query":"{ topProducts { reviews { id } } }","variables":{}}))
        .build();
    let (_trace_id, response) = router.execute_query(query).await;
    assert_eq!(response.status(), 200);

    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        body["data"]["topProducts"]
            .as_array()
            .expect("topProducts should be array")
            .len(),
        NUM_PRODUCTS
    );

    // Execute the second query - this should use the cache
    let query = Query::builder()
        .body(json!({"query":"{ topProducts { reviews { id } } }","variables":{}}))
        .build();
    let (_trace_id, response) = router.execute_query(query).await;
    assert_eq!(response.status(), 200);

    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        body["data"]["topProducts"]
            .as_array()
            .expect("topProducts should be array")
            .len(),
        NUM_PRODUCTS
    );

    // Execute more queries to ensure Redis is used and metrics are generated
    for _ in 0..5 {
        let query = Query::builder()
            .body(json!({"query":"{ topProducts { reviews { id } } }","variables":{}}))
            .build();
        let (_trace_id, response) = router.execute_query(query).await;
        assert_eq!(response.status(), 200);
    }

    // Wait a bit to ensure metrics are collected and Redis connections are established
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Assert basic Redis connection metrics (these are emitted immediately when connections are established)
    // We expect exactly 1 Redis connection for the entity cache
    router
        .assert_metrics_contains(
            r#"apollo_router_cache_redis_connections{kind="entity",otel_scope_name="apollo/router"} 1"#,
            None,
        )
        .await;

    // Assert Redis redelivery count metric (counter)
    // Should be 0 in a successful test scenario (no connection issues)
    router
        .assert_metrics_contains(
            r#"apollo_router_cache_redis_redelivery_count_total{kind="entity",otel_scope_name="apollo/router"} 0"#,
            None,
        )
        .await;

    // Assert Redis commands executed metric (counter)
    // We executed 7 queries (1 initial + 1 second + 5 more), each with cache operations
    // Based on actual test run, we expect 16 Redis commands to be executed
    router
        .assert_metrics_contains(
            r#"apollo_router_cache_redis_commands_executed_total{kind="entity",otel_scope_name="apollo/router"} 16"#,
            None,
        )
        .await;

    // Assert Redis command queue length metric (gauge)
    // Should be 0 when not under load (commands processed quickly)
    router
        .assert_metrics_contains(
            r#"apollo_router_cache_redis_command_queue_length{kind="entity",otel_scope_name="apollo/router"} 0"#,
            None,
        )
        .await;

    // Note: Network latency gauge (apollo_router_cache_redis_network_latency_avg) is implemented
    // but may not emit in test environments where Redis network latency samples are not generated.
    // This is expected behavior - the gauge only emits when actual network measurements are available.
    router.graceful_shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_cache_error_metrics() {
    if !graph_os_enabled() {
        return;
    }

    // Create configuration with invalid Redis configuration to trigger errors
    let namespace = uuid::Uuid::new_v4().simple().to_string();
    let config = json!({
        "include_subgraph_errors": {
            "all": true,
        },
        "preview_entity_cache": {
            "enabled": true,
            "subgraph": {
                "all": {
                    "redis": {
                        "urls": ["redis://127.0.0.1:9999"], // Invalid port to trigger connection errors
                        "ttl": "10m",
                        "namespace": namespace,
                        "required_to_start": false, // Don't fail startup, allow errors during runtime
                        "metrics_interval": "100ms",
                    },
                },
            },
        },
        "telemetry": {
            "exporters": {
                "metrics": {
                    "prometheus": {
                        "enabled": true,
                        "listen": "127.0.0.1:0",
                        "path": "/metrics",
                    },
                },
            },
        },
        "experimental_mock_subgraphs": subgraphs_with_many_entities(10),
    });

    let mut router = IntegrationTest::builder()
        .config(serde_yaml::to_string(&config).unwrap())
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    // Execute queries that will attempt Redis operations and fail
    for _ in 0..3 {
        let query = Query::builder()
            .body(json!({"query":"{ topProducts { reviews { id } } }","variables":{}}))
            .build();
        let (_trace_id, response) = router.execute_query(query).await;
        // The query should still succeed (using fallback) even though Redis fails
        assert_eq!(response.status(), 200);
    }

    // Wait for metrics to be collected
    tokio::time::sleep(std::time::Duration::from_millis(3000)).await;

    // Assert that Redis error metrics are emitted when Redis operations fail
    // We expect an IO error when connecting to an invalid Redis port
    router
        .assert_metrics_contains(
            r#"apollo_router_cache_redis_errors_total{error_type="io",kind="entity",otel_scope_name="apollo/router"} 1"#,
            None,
        )
        .await;

    router.graceful_shutdown().await;
}
