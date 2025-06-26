use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use apollo_router::Context;
use apollo_router::MockedSubgraphs;
use apollo_router::plugin::test::MockSubgraph;
use apollo_router::services;
use apollo_router::services::router::body::from_bytes;
use apollo_router::services::supergraph;
use apollo_router::test_harness::HttpService;
use fred::prelude::Client as RedisClient;
use fred::prelude::Config as RedisConfig;
use fred::prelude::*;
use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use http::header::CACHE_CONTROL;
use http_body_util::BodyExt as _;
use indexmap::IndexMap;
use serde_json::Value;
use serde_json::json;
use tower::BoxError;
use tower::Service as _;
use tower::ServiceExt;

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
        "experimental_response_cache": {
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

#[tokio::test(flavor = "multi_thread")]
async fn response_cache_basic() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let config = RedisConfig::from_url("redis://127.0.0.1:6379").unwrap();
    let client = RedisClient::new(config, None, None, None);
    let connection_task = client.init().await.unwrap();

    let mut subgraphs = MockedSubgraphs::default();
    subgraphs.insert(
        "products",
        MockSubgraph::builder()
            .with_json(
                serde_json::json! {{"query":"{topProducts{__typename upc name}}"}},
                serde_json::json! {{"data": {
                        "topProducts": [{
                            "__typename": "Product",
                            "upc": "1",
                            "name": "chair"
                        },
                        {
                            "__typename": "Product",
                            "upc": "2",
                            "name": "table"
                        }]
                }}},
            )
            .with_header(CACHE_CONTROL, HeaderValue::from_static("public"))
            .build(),
    );
    subgraphs.insert("reviews", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on Product{reviews{body}}}}",
                "variables": {
                    "representations": [
                        { "upc": "1", "__typename": "Product" },
                        { "upc": "2", "__typename": "Product" }
                    ],
                }
            }},
            serde_json::json! {{
                "data": {
                    "_entities":[
                        {
                            "reviews": [{
                                "body": "I can sit on it"
                            }]
                        },
                        {
                            "reviews": [{
                                "body": "I can sit on it"
                            },
                            {
                                "body": "I can eat on it"
                            }]
                        }
                    ]
                }
            }},
        ).with_header(CACHE_CONTROL, HeaderValue::from_static("public")).build());

    let supergraph = apollo_router::TestHarness::builder()
        .with_subgraph_network_requests()
        .configuration_json(json!({
            "experimental_response_cache": {
                "enabled": true,
                "invalidation": {
                    "listen": "127.0.0.1:4000",
                    "path": "/invalidation"
                },
                "subgraph": {
                    "all": {
                        "enabled": false,
                        "redis": {
                            "urls": ["redis://127.0.0.1:6379"],
                            "ttl": "2s",
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
            "supergraph": {
                // TODO(@goto-bus-stop): need to update the mocks and remove this, #6013
                "generate_query_fragments": false,
            }
        }))
        .unwrap()
        .extra_plugin(subgraphs)
        .schema(include_str!("../fixtures/supergraph-auth.graphql"))
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(r#"{ topProducts { name reviews { body } } }"#)
        .method(Method::POST)
        .build()
        .unwrap();

    let response = supergraph
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();
    insta::assert_json_snapshot!(response);

    // if this is failing due to a cache key change, hook up redis-cli with the MONITOR command to see the keys being set
    let s:String = client
          .get("version:1.0:subgraph:products:type:Query:hash:30cf92cd31bc204de344385c8f6d90a53da6c9180d80e8f7979a5bc19cd96055:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c")
          .await
          .unwrap();
    let v: Value = serde_json::from_str(&s).unwrap();
    insta::assert_json_snapshot!(v.as_object().unwrap().get("data").unwrap());

    let s: String = client.get("version:1.0:subgraph:reviews:type:Product:entity:72bafad9ffe61307806863b13856470e429e0cf332c99e5b735224fb0b1436f7:representation::hash:b9b8a9c94830cf56329ec2db7d7728881a6ba19cc1587710473e732e775a5870:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c").await.unwrap();
    let v: Value = serde_json::from_str(&s).unwrap();
    insta::assert_json_snapshot!(v.as_object().unwrap().get("data").unwrap());

    // we abuse the query shape to return a response with a different but overlapping set of entities
    let mut subgraphs = MockedSubgraphs::default();
    subgraphs.insert(
        "products",
        MockSubgraph::builder()
            .with_json(
                serde_json::json! {{"query":"{topProducts(first:2){__typename upc name}}"}},
                serde_json::json! {{"data": {
                        "topProducts": [{
                            "__typename": "Product",
                            "upc": "1",
                            "name": "chair"
                        },
                        {
                            "__typename": "Product",
                            "upc": "3",
                            "name": "plate"
                        }]
                }}},
            )
            .with_header(CACHE_CONTROL, HeaderValue::from_static("public"))
            .build(),
    );

    // even though the root operation returned 2 entities, we only need to get one entity from the subgraph here because
    // we already have it in cache
    subgraphs.insert("reviews", MockSubgraph::builder().with_json(
            serde_json::json!{{
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on Product{reviews{body}}}}",
                "variables": {
                    "representations": [
                        { "upc": "3", "__typename": "Product" }
                    ],
                }
            }},
            serde_json::json! {{
                "data": {
                    "_entities":[
                        {
                            "reviews": [{
                                "body": "I can eat in it"
                            }]
                        }
                    ]
                }
            }},
        ).with_header(CACHE_CONTROL, HeaderValue::from_static("public")).build());

    let supergraph = apollo_router::TestHarness::builder()
        .with_subgraph_network_requests()
        .configuration_json(json!({
            "experimental_response_cache": {
                "enabled": true,
                "invalidation": {
                    "listen": "127.0.0.1:4000",
                    "path": "/invalidation"
                },
                "subgraph": {
                    "all": {
                        "enabled": false,
                        "redis": {
                            "urls": ["redis://127.0.0.1:6379"],
                            "ttl": "2s"
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
            "supergraph": {
                // TODO(@goto-bus-stop): need to update the mocks and remove this, #6013
                "generate_query_fragments": false,
            }
        }))
        .unwrap()
        .extra_plugin(subgraphs.clone())
        .schema(include_str!("../fixtures/supergraph-auth.graphql"))
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(r#"{ topProducts(first: 2) { name reviews { body } } }"#)
        .method(Method::POST)
        .build()
        .unwrap();

    let response = supergraph
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();
    insta::assert_json_snapshot!(response);

    let s:String = client
        .get("version:1.0:subgraph:reviews:type:Product:entity:080fc430afd3fb953a05525a6a00999226c34436466eff7ace1d33d004adaae3:representation::hash:b9b8a9c94830cf56329ec2db7d7728881a6ba19cc1587710473e732e775a5870:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c")
        .await
        .unwrap();
    let v: Value = serde_json::from_str(&s).unwrap();
    insta::assert_json_snapshot!(v.as_object().unwrap().get("data").unwrap());

    const SECRET_SHARED_KEY: &str = "supersecret";
    let http_service = apollo_router::TestHarness::builder()
        .with_subgraph_network_requests()
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
                            "ttl": "2s"
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
            "supergraph": {
                // TODO(@goto-bus-stop): need to update the mocks and remove this, #6013
                "generate_query_fragments": false,
            }
        }))
        .unwrap()
        .extra_plugin(subgraphs.clone())
        .schema(include_str!("../fixtures/supergraph-auth.graphql"))
        .build_http_service()
        .await
        .unwrap();

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
        .body(from_bytes(
            serde_json::to_vec(&vec![json!({
                "subgraph": "reviews",
                "kind": "entity",
                "type": "Product",
                "key": {
                    "upc": "3"
                }
            })])
            .unwrap(),
        ))
        .unwrap();
    let response = http_service.oneshot(request).await.unwrap();
    let response_status = response.status();
    let mut resp: serde_json::Value = serde_json::from_str(
        &apollo_router::services::router::body::into_string(response.into_body())
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
    assert!(client
        .get::<String, _>("version:1.0:subgraph:reviews:type:Product:entity:080fc430afd3fb953a05525a6a00999226c34436466eff7ace1d33d004adaae3:representation::hash:b9b8a9c94830cf56329ec2db7d7728881a6ba19cc1587710473e732e775a5870:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c")
        .await.is_err());
    // This entry should still be in redis because we didn't invalidate this entry
    assert!(client
          .get::<String, _>("version:1.0:subgraph:products:type:Query:hash:30cf92cd31bc204de344385c8f6d90a53da6c9180d80e8f7979a5bc19cd96055:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c")
          .await.is_ok());

    client.quit().await.unwrap();
    // calling quit ends the connection and event listener tasks
    let _ = connection_task.await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn response_cache_with_nested_field_set() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let schema = include_str!("../../src/testdata/supergraph_nested_fields.graphql");

    let config = RedisConfig::from_url("redis://127.0.0.1:6379").unwrap();
    let client = RedisClient::new(config, None, None, None);
    let connection_task = client.connect();
    client.wait_for_connect().await.unwrap();

    let subgraphs = serde_json::json!({
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
        .with_subgraph_network_requests()
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
                            "ttl": "2s",
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
        .await
        .unwrap();
    let query = "query { allProducts { name createdBy { name country { a } } } }";

    let request = supergraph::Request::fake_builder()
        .query(query)
        .method(Method::POST)
        .build()
        .unwrap();

    let response = supergraph
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();
    insta::assert_json_snapshot!(response);

    // if this is failing due to a cache key change, hook up redis-cli with the MONITOR command to see the keys being set
    let s:String = client
          .get("version:1.0:subgraph:products:type:Query:hash:bead1beb068f19990b462d5d7c17974a33b822f4b3439407a9f820ce1cb47de9:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c")
          .await
          .unwrap();
    let v: Value = serde_json::from_str(&s).unwrap();
    insta::assert_json_snapshot!(v.as_object().unwrap().get("data").unwrap());

    let s: String = client
        .get("version:1.0:subgraph:users:type:User:entity:210e26346d676046faa9fb55d459273a43e5b5397a1a056f179a3521dc5643aa:representation:7cd02a08f4ea96f0affa123d5d3f56abca20e6014e060fe5594d210c00f64b27:hash:cfc5f467f767710804724ff6a05c3f63297328cd8283316adb25f5642e1439ad:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c")
        .await
        .unwrap();
    let v: Value = serde_json::from_str(&s).unwrap();
    insta::assert_json_snapshot!(v.as_object().unwrap().get("data").unwrap());

    let supergraph = apollo_router::TestHarness::builder()
        .with_subgraph_network_requests()
        .configuration_json(json!({
            "experimental_response_cache": {
                "enabled": true,
                "invalidation": {
                    "listen": "127.0.0.1:4000",
                    "path": "/invalidation"
                },
                "subgraph": {
                    "all": {
                        "enabled": false,
                        "redis": {
                            "urls": ["redis://127.0.0.1:6379"],
                            "ttl": "2s"
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
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .method(Method::POST)
        .build()
        .unwrap();

    let response = supergraph
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();
    insta::assert_json_snapshot!(response);

    let s: String = client
        .get("version:1.0:subgraph:users:type:User:entity:210e26346d676046faa9fb55d459273a43e5b5397a1a056f179a3521dc5643aa:representation:7cd02a08f4ea96f0affa123d5d3f56abca20e6014e060fe5594d210c00f64b27:hash:cfc5f467f767710804724ff6a05c3f63297328cd8283316adb25f5642e1439ad:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c")
        .await
        .unwrap();
    let v: Value = serde_json::from_str(&s).unwrap();
    insta::assert_json_snapshot!(v.as_object().unwrap().get("data").unwrap());

    const SECRET_SHARED_KEY: &str = "supersecret";
    let http_service = apollo_router::TestHarness::builder()
        .with_subgraph_network_requests()
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
                            "ttl": "2s"
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
        .await
        .unwrap();

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
        .body(from_bytes(
            serde_json::to_vec(&vec![json!({
                "subgraph": "users",
                "kind": "entity",
                "type": "User",
                "key": {
                    "email": "test@test.com",
                    "country": {
                        "a": "France"
                    }
                }
            })])
            .unwrap(),
        ))
        .unwrap();
    let response = http_service.oneshot(request).await.unwrap();
    let response_status = response.status();
    let mut resp: serde_json::Value = serde_json::from_str(
        &apollo_router::services::router::body::into_string(response.into_body())
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
    assert!(client
        .get::<String, _>("version:1.0:subgraph:users:type:User:entity:210e26346d676046faa9fb55d459273a43e5b5397a1a056f179a3521dc5643aa:representation:7cd02a08f4ea96f0affa123d5d3f56abca20e6014e060fe5594d210c00f64b27:hash:cfc5f467f767710804724ff6a05c3f63297328cd8283316adb25f5642e1439ad:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c")
        .await.is_err());
    // This entry should still be in redis because we didn't invalidate this entry
    assert!(client
          .get::<String, _>("version:1.0:subgraph:products:type:Query:hash:bead1beb068f19990b462d5d7c17974a33b822f4b3439407a9f820ce1cb47de9:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c")
          .await.is_ok());

    client.quit().await.unwrap();
    // calling quit ends the connection and event listener tasks
    let _ = connection_task.await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn response_cache_authorization() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let config = RedisConfig::from_url("redis://127.0.0.1:6379").unwrap();
    let client = RedisClient::new(config, None, None, None);
    let connection_task = client.init().await.unwrap();

    let mut subgraphs = MockedSubgraphs::default();
    subgraphs.insert(
            "accounts",
            MockSubgraph::builder().with_json(
                serde_json::json!{{
                    "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{username}}}",
                    "variables": {
                        "representations": [
                            { "__typename": "User", "id": "1" },
                            { "__typename": "User", "id": "2" }
                        ],
                    }
                }},
                serde_json::json! {{
                    "data": {
                        "_entities":[
                            {
                                "username": "ada"
                            },
                            {
                                "username": "charles"
                            }
                        ]
                    }
                }},
            ).with_json(
                serde_json::json! {{"query":"{me{id}}"}},
                serde_json::json! {{"data": {
                    "me": {
                        "id": "1"
                    }
                }}},
            ).with_header(CACHE_CONTROL, HeaderValue::from_static("public"))
            .build(),
        );
    subgraphs.insert(
        "products",
        MockSubgraph::builder()
            .with_json(
                serde_json::json! {{"query":"{topProducts{__typename upc name}}"}},
                serde_json::json! {{"data": {
                        "topProducts": [{
                            "__typename": "Product",
                            "upc": "1",
                            "name": "chair"
                        },
                        {
                            "__typename": "Product",
                            "upc": "2",
                            "name": "table"
                        }]
                }}},
            )
            .with_header(CACHE_CONTROL, HeaderValue::from_static("public"))
            .build(),
    );
    subgraphs.insert(
            "reviews",
            MockSubgraph::builder().with_json(
                    serde_json::json!{{
                        "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on Product{reviews{body}}}}",
                        "variables": {
                            "representations": [
                                { "upc": "1", "__typename": "Product" },
                                { "upc": "2", "__typename": "Product" }
                            ],
                        }
                    }},
                    serde_json::json! {{
                        "data": {
                            "_entities":[
                                {
                                    "reviews": [{
                                        "body": "I can sit on it"
                                    }]
                                },
                                {
                                    "reviews": [{
                                        "body": "I can sit on it"
                                    },
                                    {
                                        "body": "I can eat on it"
                                    }]
                                }
                            ]
                        }
                    }},
                ).with_json(
                    serde_json::json!{{
                        "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on Product{reviews{body author{__typename id}}}}}",
                        "variables": {
                            "representations": [
                                { "upc": "1", "__typename": "Product" },
                                { "upc": "2", "__typename": "Product" }
                            ],
                        }
                    }},
                    serde_json::json! {{
                        "data": {
                            "_entities":[
                                {
                                    "reviews": [{
                                        "body": "I can sit on it",
                                        "author": {
                                            "__typename": "User",
                                            "id": "1"
                                        }
                                    }]
                                },
                                {
                                    "reviews": [{
                                        "body": "I can sit on it",
                                        "author": {
                                            "__typename": "User",
                                            "id": "1"
                                        }
                                    },
                                    {
                                        "body": "I can eat on it",
                                        "author": {
                                            "__typename": "User",
                                            "id": "2"
                                        }
                                    }]
                                }
                            ]
                        }
                    }},
                ).with_header(CACHE_CONTROL, HeaderValue::from_static("public"))
                .build(),
        );

    let supergraph = apollo_router::TestHarness::builder()
        .with_subgraph_network_requests()
        .configuration_json(json!({
            "experimental_response_cache": {
                "enabled": true,
                "invalidation": {
                    "listen": "127.0.0.1:4000",
                    "path": "/invalidation"
                },
                "subgraph": {
                    "all": {
                        "enabled": false,
                        "redis": {
                            "urls": ["redis://127.0.0.1:6379"],
                            "ttl": "2s"
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
            "authorization": {
                "preview_directives": {
                    "enabled": true
                }
            },
            "include_subgraph_errors": {
                "all": true
            },
            "supergraph": {
                // TODO(@goto-bus-stop): need to update the mocks and remove this, #6013
                "generate_query_fragments": false,
            }
        }))
        .unwrap()
        .extra_plugin(subgraphs)
        .schema(include_str!("../fixtures/supergraph-auth.graphql"))
        .build_supergraph()
        .await
        .unwrap();

    let context = Context::new();
    context
        .insert(
            "apollo::authorization::required_scopes",
            json! {["profile", "read:user", "read:name"]},
        )
        .unwrap();
    let request = supergraph::Request::fake_builder()
        .query(r#"{ me { id name } topProducts { name reviews { body author { username } } } }"#)
        .context(context)
        .method(Method::POST)
        .build()
        .unwrap();

    let response = supergraph
        .clone()
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();
    insta::assert_json_snapshot!(response);

    let s:String = client
          .get("version:1.0:subgraph:products:type:Query:hash:30cf92cd31bc204de344385c8f6d90a53da6c9180d80e8f7979a5bc19cd96055:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c")
          .await
          .unwrap();
    let v: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(
        v.as_object().unwrap().get("data").unwrap(),
        &json! {{
            "topProducts": [{
                "__typename": "Product",
                "upc": "1",
                "name": "chair"
            },
            {
                "__typename": "Product",
                "upc": "2",
                "name": "table"
            }]
        }}
    );

    let s: String = client
        .get("version:1.0:subgraph:reviews:type:Product:entity:72bafad9ffe61307806863b13856470e429e0cf332c99e5b735224fb0b1436f7:representation::hash:b9b8a9c94830cf56329ec2db7d7728881a6ba19cc1587710473e732e775a5870:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c")
        .await
        .unwrap();
    let v: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(
        v.as_object().unwrap().get("data").unwrap(),
        &json! {{
            "reviews": [
                {"body": "I can sit on it"}
            ]
        }}
    );

    let context = Context::new();
    context
        .insert(
            "apollo::authorization::required_scopes",
            json! {["profile", "read:user", "read:name"]},
        )
        .unwrap();
    context
        .insert(
            "apollo::authentication::jwt_claims",
            json! {{ "scope": "read:user read:name" }},
        )
        .unwrap();
    let request = supergraph::Request::fake_builder()
        .query(r#"{ me { id name } topProducts { name reviews { body author { username } } } }"#)
        .context(context)
        .method(Method::POST)
        .build()
        .unwrap();

    let response = supergraph
        .clone()
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();
    insta::assert_json_snapshot!(response);

    let s:String = client
          .get("version:1.0:subgraph:reviews:type:Product:entity:72bafad9ffe61307806863b13856470e429e0cf332c99e5b735224fb0b1436f7:representation::hash:572a2cdde770584306cd0def24555773323b1738e9a303c7abc1721b6f9f2ec4:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c")
          .await
          .unwrap();
    let v: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(
        v.as_object().unwrap().get("data").unwrap(),
        &json! {{
            "reviews": [{
                "body": "I can sit on it",
                "author": {"__typename": "User", "id": "1"}
            }]
        }}
    );
    let s:String = client
          .get("version:1.0:subgraph:reviews:type:Product:entity:472484d4df9e800bbb846447c4c077787860c4c9ec59579d50009bfcba275c3b:representation::hash:572a2cdde770584306cd0def24555773323b1738e9a303c7abc1721b6f9f2ec4:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c")
          .await
          .unwrap();
    let v: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(
        v.as_object().unwrap().get("data").unwrap(),
        &json! {{
            "reviews": [{
                "body": "I can sit on it",
                "author": {
                    "__typename": "User",
                    "id": "1"
                }
            },
            {
                "body": "I can eat on it",
                "author": {
                    "__typename": "User",
                    "id": "2"
                }
            }]
        }}
    );

    let context = Context::new();
    context
        .insert(
            "apollo::authorization::required_scopes",
            json! {["profile", "read:user", "read:name"]},
        )
        .unwrap();
    context
        .insert(
            "apollo::authentication::jwt_claims",
            json! {{ "scope": "read:user profile" }},
        )
        .unwrap();
    let request = supergraph::Request::fake_builder()
        .query(r#"{ me { id name } topProducts { name reviews { body author { username } } } }"#)
        .context(context)
        .method(Method::POST)
        .build()
        .unwrap();

    let response = supergraph
        .clone()
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap();
    insta::assert_json_snapshot!(response);

    client.quit().await.unwrap();
    // calling quit ends the connection and event listener tasks
    let _ = connection_task.await;
    Ok(())
}
