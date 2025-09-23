use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use apollo_router::graphql;
use apollo_router::services::router;
use apollo_router::services::supergraph;
use apollo_router::test_harness::HttpService;
use fred::clients::Client;
use fred::interfaces::ClientLike;
use fred::interfaces::KeysInterface;
use fred::types::Builder;
use http::HeaderMap;
use http::HeaderValue;
use http_body_util::BodyExt as _;
use indexmap::IndexMap;
use serde_json::Value;
use serde_json::json;
use tokio::time::sleep;
use tokio_util::future::FutureExt;
use tower::BoxError;
use tower::Service as _;
use tower::ServiceExt as _;

use crate::integration::common::graph_os_enabled;

const REDIS_URL: &str = "redis://127.0.0.1:6379";
const INVALIDATION_PATH: &str = "/invalidation";
const INVALIDATION_SHARED_KEY: &str = "supersecret";

/// Isolate tests from each other by adding a random redis key prefix
pub(crate) fn namespace() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

async fn redis_client() -> Result<Client, BoxError> {
    let client =
        Builder::from_config(fred::prelude::Config::from_url(REDIS_URL).unwrap()).build()?;
    client.init().await?;
    Ok(client)
}

fn base_config() -> Value {
    json!({
        "include_subgraph_errors": {
            "all": true,
        },
        "experimental_response_cache": {
            "enabled": true,
            "subgraph": {
                "all": {
                    "redis": {
                        "urls": ["redis://127.0.0.1:6379"],
                        "pool_size": 3,
                        "namespace": namespace(),
                        "required_to_start": true,
                    },
                    "ttl": "10m",
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

fn failure_config() -> Value {
    json!({
        "include_subgraph_errors": {
            "all": true,
        },
        "experimental_response_cache": {
            "enabled": true,
            "subgraph": {
                "all": {
                    "redis": {
                        "urls": ["redis://invalid"],
                        "pool_size": 3,
                        "namespace": namespace(),
                        "required_to_start": false,
                    },
                    "ttl": "10m",
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

fn base_subgraphs() -> Value {
    json!({
        "products": {
            "headers": {"cache-control": "public"},
            "query": {
                "topProducts": [
                    {"upc": "1", "__cacheTags": ["topProducts"]},
                    {"upc": "2"},
                ],
            },
        },
        "reviews": {
            "headers": {"cache-control": "public"},
            "entities": [
                {
                    "__cacheTags": ["product-1"],
                    "__typename": "Product",
                    "upc": "1",
                    "reviews": [{"id": "r1a"}, {"id": "r1b"}],
                },
                {
                    "__cacheTags": ["product-2"],
                    "__typename": "Product",
                    "upc": "2",
                    "reviews": [{"id": "r2"}],
                },
            ],
        },
    })
}

async fn harness(
    mut config: Value,
    subgraphs: Value,
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

fn graphql_request(query: &str) -> router::Request {
    supergraph::Request::fake_builder()
        .query(query)
        .build()
        .unwrap()
        .try_into()
        .unwrap()
}

async fn make_json_request(
    router: &mut HttpService,
    request: http::Request<Value>,
) -> (HeaderMap<String>, Value) {
    let request = request.map(|body| router::body::from_bytes(serde_json::to_vec(&body).unwrap()));
    make_http_request(router, request).await
}

async fn make_http_request<ResponseBody>(
    router: &mut HttpService,
    request: http::Request<router::Body>,
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

#[tokio::test(flavor = "multi_thread")]
async fn basic_cache_skips_subgraph_request() {
    if !graph_os_enabled() {
        return;
    }

    let (mut router, subgraph_request_counters) = harness(base_config(), base_subgraphs()).await;
    insta::assert_yaml_snapshot!(subgraph_request_counters, @r"
    products: 0
    reviews: 0
    ");
    let (headers, body) = make_graphql_request(&mut router).await;
    assert!(headers["cache-control"].contains("public"));
    insta::assert_yaml_snapshot!(body, @r"
    data:
      topProducts:
        - reviews:
            - id: r1a
            - id: r1b
        - reviews:
            - id: r2
    ");
    insta::assert_yaml_snapshot!(subgraph_request_counters, @r"
    products: 1
    reviews: 1
    ");
    // Needed because insert in the cache is async
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (headers, body) = make_graphql_request(&mut router).await;
    assert!(headers["cache-control"].contains("public"));
    insta::assert_yaml_snapshot!(body, @r"
    data:
      topProducts:
        - reviews:
            - id: r1a
            - id: r1b
        - reviews:
            - id: r2
    ");
    // Unchanged, everything is in cache so we don’t need to make more subgraph requests:
    insta::assert_yaml_snapshot!(subgraph_request_counters, @r"
    products: 1
    reviews: 1
    ");
}

#[tokio::test(flavor = "multi_thread")]
async fn no_failure_when_storage_unavailable() {
    if !graph_os_enabled() {
        return;
    }

    let (mut router, subgraph_request_counters) = harness(failure_config(), base_subgraphs()).await;
    insta::assert_yaml_snapshot!(subgraph_request_counters, @r"
    products: 0
    reviews: 0
    ");
    let (headers, body) = make_graphql_request(&mut router).await;
    assert!(headers["cache-control"].contains("public"));
    insta::assert_yaml_snapshot!(body, @r"
    data:
      topProducts:
        - reviews:
            - id: r1a
            - id: r1b
        - reviews:
            - id: r2
    ");
    insta::assert_yaml_snapshot!(subgraph_request_counters, @r"
    products: 1
    reviews: 1
    ");
    let (headers, body) = make_graphql_request(&mut router).await;
    assert!(headers["cache-control"].contains("public"));
    insta::assert_yaml_snapshot!(body, @r"
    data:
      topProducts:
        - reviews:
            - id: r1a
            - id: r1b
        - reviews:
            - id: r2
    ");
    // Would have been unchanged because both subgraph requests were cacheable,
    // but cache storage isn’t available to we fall back to calling the subgraph again
    insta::assert_yaml_snapshot!(subgraph_request_counters, @r"
    products: 2
    reviews: 2
    ");
}

#[tokio::test(flavor = "multi_thread")]
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
    insta::assert_yaml_snapshot!(subgraph_request_counters, @r"
    products: 0
    reviews: 0
    ");
    let (headers, body) = make_graphql_request(&mut router).await;
    // When subgraphs don’t set a cache-control header, Router defaults to not caching
    // and instructs any downstream cache to do the same:
    assert_eq!(headers["cache-control"], "no-store");
    insta::assert_yaml_snapshot!(body, @r"
    data:
      topProducts:
        - reviews:
            - id: r1a
            - id: r1b
        - reviews:
            - id: r2
    ");
    insta::assert_yaml_snapshot!(subgraph_request_counters, @r"
    products: 1
    reviews: 1
    ");
    // Needed because insert in the cache is async
    tokio::time::sleep(Duration::from_millis(100)).await;

    let (headers, body) = make_graphql_request(&mut router).await;
    assert_eq!(headers["cache-control"], "no-store");
    insta::assert_yaml_snapshot!(body, @r"
    data:
      topProducts:
        - reviews:
            - id: r1a
            - id: r1b
        - reviews:
            - id: r2
    ");
    // More supergraph requsets lead to more subgraph requests:
    insta::assert_yaml_snapshot!(subgraph_request_counters, @r"
    products: 2
    reviews: 2
    ");
}

#[tokio::test(flavor = "multi_thread")]
async fn invalidate_with_endpoint_by_type() {
    if !graph_os_enabled() {
        return;
    }

    let (mut router, subgraph_request_counters) = harness(base_config(), base_subgraphs()).await;
    let (headers, body) = make_graphql_request(&mut router).await;
    assert!(headers["cache-control"].contains("public"));
    assert!(body.errors.is_empty());
    insta::assert_yaml_snapshot!(subgraph_request_counters, @r"
    products: 1
    reviews: 1
    ");
    let request = http::Request::builder()
        .method("POST")
        .uri(INVALIDATION_PATH)
        .header("Authorization", INVALIDATION_SHARED_KEY)
        .body(json!([{
            "kind": "type",
            "subgraph": "reviews",
            "type": "Product"
        }]))
        .unwrap();
    // Needed because insert in the cache is async
    for i in 0..10 {
        let (_headers, body) = make_json_request(&mut router, request.clone()).await;
        let expected_value = json!({"count": 2});

        if body == expected_value {
            break;
        } else if i == 9 {
            insta::assert_yaml_snapshot!(body, @"count: 2");
        }
    }

    let (headers, body) = make_graphql_request(&mut router).await;
    assert!(headers["cache-control"].contains("public"));
    assert!(body.errors.is_empty());
    // After invalidation, reviews need to be requested again but products are still in cache:
    insta::assert_yaml_snapshot!(subgraph_request_counters, @r"
    products: 1
    reviews: 2
    ");
}

#[tokio::test(flavor = "multi_thread")]
async fn invalidate_with_endpoint_by_entity_cache_tag() {
    if !graph_os_enabled() {
        return;
    }

    let (mut router, subgraph_request_counters) = harness(base_config(), base_subgraphs()).await;
    let (headers, body) = make_graphql_request(&mut router).await;
    assert!(headers["cache-control"].contains("public"));
    assert!(body.errors.is_empty());
    insta::assert_yaml_snapshot!(subgraph_request_counters, @r"
    products: 1
    reviews: 1
    ");

    let request = http::Request::builder()
        .method("POST")
        .uri(INVALIDATION_PATH)
        .header("Authorization", INVALIDATION_SHARED_KEY)
        .body(json!([{
            "kind": "cache_tag",
            "subgraphs": ["reviews"],
            "cache_tag": "product-1",
        }]))
        .unwrap();
    // Needed because insert in the cache is async
    for i in 0..10 {
        let (_headers, body) = make_json_request(&mut router, request.clone()).await;
        let expected_value = json!({"count": 1});

        if body == expected_value {
            break;
        } else if i == 9 {
            insta::assert_yaml_snapshot!(body, @"count: 1");
        }
    }
    let (headers, body) = make_graphql_request(&mut router).await;
    assert!(headers["cache-control"].contains("public"));
    assert!(body.errors.is_empty());
    // After invalidation, reviews need to be requested again but products are still in cache:
    insta::assert_yaml_snapshot!(subgraph_request_counters, @r"
    products: 1
    reviews: 2
    ");
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

    tokio::time::sleep(Duration::from_secs(2)).await;

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

    tokio::time::sleep(Duration::from_secs(2)).await;

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

macro_rules! check_cache_key {
    ($namespace: expr, $cache_key: expr, $client: expr) => {
        let mut record: Option<String> = None;
        let key = format!("{}:{}", $namespace, $cache_key);
        // Retry a few times because insert is asynchronous
        for _ in 0..10 {
            match $client
                .get(key.clone())
                .timeout(Duration::from_secs(5))
                .await
            {
                Ok(Ok(resp)) => {
                    record = Some(resp);
                    break;
                }
                Ok(Err(_)) => {
                    sleep(Duration::from_secs(1)).await;
                }
                Err(_) => {
                    panic!("long timeout connecting to redis - did you call client.init()?");
                }
            }
        }

        match record {
            Some(s) => {
                let cache_value: Value = serde_json::from_str(&s).unwrap();
                let v: Value = cache_value.get("data").unwrap().clone();
                insta::assert_json_snapshot!(v);
            }
            None => panic!("cannot get cache key {}", $cache_key),
        }
    };
}

async fn cache_key_exists(
    namespace: &str,
    cache_key: &str,
    client: &Client,
) -> Result<bool, fred::error::Error> {
    let key = format!("{namespace}:{cache_key}");
    let count: u32 = client.exists(key).await?;
    Ok(count == 1)
}

#[tokio::test(flavor = "multi_thread")]
async fn integration_test_basic() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let namespace = namespace();
    let client = redis_client().await?;

    let subgraphs = json!({
        "products": {
            "query": {"topProducts": [{
                "__typename": "Product",
                "upc": "1",
                "name": "chair"
            },
            {
                "__typename": "Product",
                "upc": "2",
                "name": "table"
            },
            {
                "__typename": "Product",
                "upc": "3",
                "name": "plate"
            }]},
            "headers": {"cache-control": "public"},
        },
        "reviews": {
            "entities": [{
                "__typename": "Product",
                "upc": "1",
                "reviews": [{
                    "__typename": "Review",
                    "body": "I can sit on it",
                }]
            },
            {
                "__typename": "Product",
                "upc": "2",
                "reviews": [{
                    "__typename": "Review",
                    "body": "I can sit on it",
                }, {
                    "__typename": "Review",
                    "body": "I can sit on it2",
                }]
            },
            {
                "__typename": "Product",
                "upc": "3",
                "reviews": [{
                    "__typename": "Review",
                    "body": "I can sit on it",
                }, {
                    "__typename": "Review",
                    "body": "I can sit on it2",
                }, {
                    "__typename": "Review",
                    "body": "I can sit on it3",
                }]
            }],
            "headers": {"cache-control": "public"},
        }
    });
    let supergraph = apollo_router::TestHarness::builder()
        .configuration_json(json!({
            "experimental_response_cache": {
                "enabled": true,
                "debug": true,
                "invalidation": {
                    "listen": "127.0.0.1:4000",
                    "path": "/invalidation"
                },
                "subgraph": {
                    "all": {
                        "enabled": true,
                        "redis": {
                            "urls": ["redis://127.0.0.1:6379"],
                            "namespace": namespace,
                            "pool_size": 3
                        },
                    },
                    "subgraphs": {
                        "products": {
                            "enabled": true,
                            "ttl": "60s"
                        },
                        "reviews": {
                            "enabled": true,
                            "ttl": "10s"
                        }
                    }
                }
            },
            "include_subgraph_errors": {
                "all": true
            },
            "experimental_mock_subgraphs": subgraphs.clone()
        }))
        .unwrap()
        .schema(include_str!("../fixtures/supergraph-auth.graphql"))
        .build_supergraph()
        .await?;

    let request = supergraph::Request::fake_builder()
        .query(r#"{ topProducts { name reviews { body } } }"#)
        .method(http::Method::POST)
        .header("apollo-cache-debugging", "true")
        .build()?;

    let response = supergraph
        .oneshot(request)
        .await?
        .next_response()
        .await
        .unwrap();

    insta::assert_json_snapshot!(response, {
        ".extensions.apolloCacheDebugging.data[].cacheControl.created" => 0
    });

    let cache_key = "version:1.0:subgraph:products:type:Query:hash:bf44683f0c222652b509d6efb8f324610c8671181de540a96a5016bd71daa7cc:data:070af9367f9025bd796a1b7e0cd1335246f658aa4857c3a4d6284673b7d07fa6";
    check_cache_key!(&namespace, cache_key, &client);

    let cache_key = "version:1.0:subgraph:reviews:type:Product:entity:cf4952a1e511b1bf2561a6193b4cdfc95f265a79e5cae4fd3e46fd9e75bc512f:representation::hash:06a24c8b3861c95f53d224071ee9627ee81b4826d23bc3de69bdc0031edde6ed:data:070af9367f9025bd796a1b7e0cd1335246f658aa4857c3a4d6284673b7d07fa6";
    check_cache_key!(&namespace, cache_key, &client);

    let supergraph = apollo_router::TestHarness::builder()
        .configuration_json(json!({
            "experimental_response_cache": {
                "enabled": true,
                "debug": true,
                "invalidation": {
                    "listen": "127.0.0.1:4000",
                    "path": "/invalidation"
                },
                "subgraph": {
                    "all": {
                        "enabled": false,
                        "redis": {
                            "urls": ["redis://127.0.0.1:6379"],
                            "namespace": namespace,
                        },
                    },
                    "subgraphs": {
                        "products": {
                            "enabled": true,
                            "ttl": "60s"
                        },
                        "reviews": {
                            "enabled": true,
                            "ttl": "10s"
                        }
                    }
                }
            },
            "include_subgraph_errors": {
                "all": true
            },
            "experimental_mock_subgraphs": subgraphs.clone()
        }))
        .unwrap()
        .schema(include_str!("../fixtures/supergraph-auth.graphql"))
        .build_supergraph()
        .await?;

    let request = supergraph::Request::fake_builder()
        .query(r#"{ topProducts(first: 2) { name reviews { body } } }"#)
        .header("apollo-cache-debugging", "true")
        .method(http::Method::POST)
        .build()?;

    let response = supergraph
        .oneshot(request)
        .await?
        .next_response()
        .await
        .unwrap();
    insta::assert_json_snapshot!(response, {
        ".extensions.apolloCacheDebugging.data[].cacheControl.created" => 0
    });

    let cache_key = "version:1.0:subgraph:reviews:type:Product:entity:cf4952a1e511b1bf2561a6193b4cdfc95f265a79e5cae4fd3e46fd9e75bc512f:representation::hash:06a24c8b3861c95f53d224071ee9627ee81b4826d23bc3de69bdc0031edde6ed:data:070af9367f9025bd796a1b7e0cd1335246f658aa4857c3a4d6284673b7d07fa6";
    check_cache_key!(&namespace, cache_key, &client);

    const SECRET_SHARED_KEY: &str = "supersecret";
    let http_service = apollo_router::TestHarness::builder()
        .configuration_json(json!({
            "experimental_response_cache": {
                "enabled": true,
                "invalidation": {
                    "listen": "127.0.0.1:4000",
                    "path": "/invalidation"
                },
                "subgraph": {
                    "all": {
                        "enabled": true,
                        "redis": {
                            "urls": ["redis://127.0.0.1:6379"],
                            "namespace": namespace,
                        },
                        "invalidation": {
                            "enabled": true,
                            "shared_key": SECRET_SHARED_KEY
                        }
                    },
                    "subgraphs": {
                        "products": {
                            "enabled": true,
                            "ttl": "60s",
                            "invalidation": {
                                "enabled": true,
                                "shared_key": SECRET_SHARED_KEY
                            }
                        },
                        "reviews": {
                            "enabled": true,
                            "ttl": "10s",
                            "invalidation": {
                                "enabled": true,
                                "shared_key": SECRET_SHARED_KEY
                            }
                        }
                    }
                }
            },
            "include_subgraph_errors": {
                "all": true
            },
            "experimental_mock_subgraphs": subgraphs.clone()
        }))
        .unwrap()
        .schema(include_str!("../fixtures/supergraph-auth.graphql"))
        .build_http_service()
        .await?;

    let request = http::Request::builder()
        .uri("http://127.0.0.1:4000/invalidation")
        .method(http::Method::POST)
        .header(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )
        .header(
            http::header::AUTHORIZATION,
            HeaderValue::from_static(SECRET_SHARED_KEY),
        )
        .body(router::body::from_bytes(
            serde_json::to_vec(&vec![json!({
                "subgraph": "reviews",
                "kind": "type",
                "type": "Product"
            })])
            .unwrap(),
        ))
        .unwrap();
    let response = http_service.oneshot(request).await.unwrap();
    let response_status = response.status();
    let mut resp: Value = serde_json::from_str(
        &router::body::into_string(response.into_body())
            .await
            .unwrap(),
    )
    .unwrap();

    assert_eq!(
        resp.as_object_mut()
            .unwrap()
            .get("count")
            .unwrap()
            .as_u64()
            .unwrap(),
        3u64
    );
    assert!(response_status.is_success());

    // This should be in error because we invalidated this entity
    let cache_key = "version:1.0:subgraph:reviews:type:Product:entity:cf4952a1e511b1bf2561a6193b4cdfc95f265a79e5cae4fd3e46fd9e75bc512f:representation::hash:06a24c8b3861c95f53d224071ee9627ee81b4826d23bc3de69bdc0031edde6ed:data:070af9367f9025bd796a1b7e0cd1335246f658aa4857c3a4d6284673b7d07fa6";
    assert!(!cache_key_exists(&namespace, &cache_key, &client).await?);

    // This entry should still be in redis because we didn't invalidate this entry
    let cache_key = "version:1.0:subgraph:products:type:Query:hash:bf44683f0c222652b509d6efb8f324610c8671181de540a96a5016bd71daa7cc:data:070af9367f9025bd796a1b7e0cd1335246f658aa4857c3a4d6284673b7d07fa6";
    assert!(cache_key_exists(&namespace, &cache_key, &client).await?);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn integration_test_with_nested_field_set() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let namespace = namespace();
    let schema = include_str!("../../src/testdata/supergraph_nested_fields.graphql");

    let client = redis_client().await?;

    let subgraphs = json!({
        "products": {
            "query": {"allProducts": [{
                "id": "1",
                "name": "Test",
                "sku": "150",
                "createdBy": { "__typename": "User", "email": "test@test.com", "country": {"a": "France"} }
            }]},
            "headers": {"cache-control": "public"},
        },
        "users": {
            "entities": [{
                "__typename": "User",
                "email": "test@test.com",
                "name": "test",
                "country": {
                    "a": "France"
                }
            }],
            "headers": {"cache-control": "public"},
        }
    });

    let supergraph = apollo_router::TestHarness::builder()
        .configuration_json(json!({
            "experimental_response_cache": {
                "enabled": true,
                "debug": true,
                "invalidation": {
                    "listen": "127.0.0.1:4000",
                    "path": "/invalidation"
                },
                "subgraph": {
                    "all": {
                        "enabled": true,
                        "redis": {
                            "urls": ["redis://127.0.0.1:6379"],
                            "namespace": namespace,
                            "pool_size": 3
                        },
                    }
                }
            },
            "include_subgraph_errors": {
                "all": true
            },
            "experimental_mock_subgraphs": subgraphs.clone()
        }))
        .unwrap()
        .schema(schema)
        .build_supergraph()
        .await?;
    let query = "query { allProducts { name createdBy { name country { a } } } }";

    let request = supergraph::Request::fake_builder()
        .query(query)
        .header("apollo-cache-debugging", "true")
        .method(http::Method::POST)
        .build()?;

    let response = supergraph
        .oneshot(request)
        .await?
        .next_response()
        .await
        .unwrap();
    insta::assert_json_snapshot!(response, {
        ".extensions.apolloCacheDebugging.data[].cacheControl.created" => 0
    });

    let cache_key = "version:1.0:subgraph:products:type:Query:hash:f4f41cfa309494d41648c3a3c398c61cb00197696102199454a25a0dcdd2f592:data:070af9367f9025bd796a1b7e0cd1335246f658aa4857c3a4d6284673b7d07fa6";
    check_cache_key!(&namespace, cache_key, &client);

    let cache_key = "version:1.0:subgraph:users:type:User:entity:b41dfad85edaabac7bb681098e9b23e21b3b8b9b8b1849babbd5a1300af64b43:representation:68fd4df7c06fd234bd0feb24e3300abcc06136ea8a9dd7533b7378f5fce7cfc4:hash:460b70e698b8c9d8496b0567e0f0848b9f7fef36e841a8a0b0771891150c35e5:data:070af9367f9025bd796a1b7e0cd1335246f658aa4857c3a4d6284673b7d07fa6";
    check_cache_key!(&namespace, cache_key, &client);

    let supergraph = apollo_router::TestHarness::builder()
        .configuration_json(json!({
            "experimental_response_cache": {
                "enabled": true,
                "debug": true,
                "invalidation": {
                    "listen": "127.0.0.1:4000",
                    "path": "/invalidation"
                },
                "subgraph": {
                    "all": {
                        "enabled": false,
                        "redis": {
                            "urls": ["redis://127.0.0.1:6379"],
                            "namespace": namespace,
                        },
                    },
                    "subgraphs": {
                        "products": {
                            "enabled": true,
                            "ttl": "60s"
                        },
                        "reviews": {
                            "enabled": true,
                            "ttl": "10s"
                        }
                    }
                }
            },
            "include_subgraph_errors": {
                "all": true
            },
            "experimental_mock_subgraphs": subgraphs.clone()
        }))
        .unwrap()
        .schema(schema)
        .build_supergraph()
        .await?;

    let request = supergraph::Request::fake_builder()
        .query(query)
        .method(http::Method::POST)
        .build()?;

    let response = supergraph
        .oneshot(request)
        .await?
        .next_response()
        .await
        .unwrap();
    insta::assert_json_snapshot!(response, {
        ".extensions.apolloCacheDebugging.data[].cacheControl.created" => 0
    });

    let cache_key = "version:1.0:subgraph:users:type:User:entity:b41dfad85edaabac7bb681098e9b23e21b3b8b9b8b1849babbd5a1300af64b43:representation:68fd4df7c06fd234bd0feb24e3300abcc06136ea8a9dd7533b7378f5fce7cfc4:hash:460b70e698b8c9d8496b0567e0f0848b9f7fef36e841a8a0b0771891150c35e5:data:070af9367f9025bd796a1b7e0cd1335246f658aa4857c3a4d6284673b7d07fa6";
    check_cache_key!(&namespace, cache_key, &client);

    const SECRET_SHARED_KEY: &str = "supersecret";
    let http_service = apollo_router::TestHarness::builder()
        .configuration_json(json!({
            "experimental_response_cache": {
                "enabled": true,
                "debug": true,
                "invalidation": {
                    "listen": "127.0.0.1:4000",
                    "path": "/invalidation"
                },
                "subgraph": {
                    "all": {
                        "enabled": true,
                        "redis": {
                            "urls": ["redis://127.0.0.1:6379"],
                            "namespace": namespace,
                        },
                        "invalidation": {
                            "enabled": true,
                            "shared_key": SECRET_SHARED_KEY
                        }
                    },
                    "subgraphs": {
                        "products": {
                            "enabled": true,
                            "ttl": "60s",
                            "invalidation": {
                                "enabled": true,
                                "shared_key": SECRET_SHARED_KEY
                            }
                        },
                        "reviews": {
                            "enabled": true,
                            "ttl": "10s",
                            "invalidation": {
                                "enabled": true,
                                "shared_key": SECRET_SHARED_KEY
                            }
                        }
                    }
                }
            },
            "include_subgraph_errors": {
                "all": true
            },
            "experimental_mock_subgraphs": subgraphs.clone()
        }))
        .unwrap()
        .schema(schema)
        .build_http_service()
        .await?;

    let request = http::Request::builder()
        .uri("http://127.0.0.1:4000/invalidation")
        .method(http::Method::POST)
        .header(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )
        .header(
            http::header::AUTHORIZATION,
            HeaderValue::from_static(SECRET_SHARED_KEY),
        )
        .body(router::body::from_bytes(
            serde_json::to_vec(&vec![json!({
                "subgraph": "users",
                "kind": "type",
                "type": "User"
            })])
            .unwrap(),
        ))
        .unwrap();
    let response = http_service.oneshot(request).await.unwrap();
    let response_status = response.status();
    let mut resp: Value = serde_json::from_str(
        &router::body::into_string(response.into_body())
            .await
            .unwrap(),
    )
    .unwrap();

    assert_eq!(
        resp.as_object_mut()
            .unwrap()
            .get("count")
            .unwrap()
            .as_u64()
            .unwrap(),
        1u64
    );
    assert!(response_status.is_success());

    // This should be in error because we invalidated this entity
    let cache_key = "version:1.0:subgraph:users:type:User:entity:b41dfad85edaabac7bb681098e9b23e21b3b8b9b8b1849babbd5a1300af64b43:representation:68fd4df7c06fd234bd0feb24e3300abcc06136ea8a9dd7533b7378f5fce7cfc4:hash:460b70e698b8c9d8496b0567e0f0848b9f7fef36e841a8a0b0771891150c35e5:data:070af9367f9025bd796a1b7e0cd1335246f658aa4857c3a4d6284673b7d07fa6";
    assert!(!cache_key_exists(&namespace, &cache_key, &client).await?);

    // This entry should still be in redis because we didn't invalidate this entry
    let cache_key = "version:1.0:subgraph:products:type:Query:hash:f4f41cfa309494d41648c3a3c398c61cb00197696102199454a25a0dcdd2f592:data:070af9367f9025bd796a1b7e0cd1335246f658aa4857c3a4d6284673b7d07fa6";
    assert!(cache_key_exists(&namespace, &cache_key, &client).await?);

    Ok(())
}
