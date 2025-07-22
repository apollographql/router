use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use apollo_router::services;
use apollo_router::test_harness::HttpService;
use http::HeaderMap;
use http_body_util::BodyExt as _;
use indexmap::IndexMap;
use serde_json::json;
use tower::Service as _;
use tower::ServiceExt as _;

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
    config.as_object_mut().unwrap().insert(
        "experimental_mock_subgraphs".into(),
        json!({
            "static_subgraphs": subgraphs
        }),
    );
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

async fn make_graphql_request(
    router: &mut HttpService,
) -> (HeaderMap<String>, apollo_router::graphql::Response) {
    let query = "{ topProducts { reviews { id } } }";
    let request: services::router::Request = services::supergraph::Request::fake_builder()
        .query(query)
        .build()
        .unwrap()
        .try_into()
        .unwrap();
    make_http_request(router, request.into()).await
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
