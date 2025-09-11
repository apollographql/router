use std::time::Duration;

use apollo_router::services::router::body::from_bytes;
use apollo_router::services::supergraph;
use fred::interfaces::ClientLike;
use fred::interfaces::KeysInterface;
use fred::prelude::Builder;
use fred::prelude::Config;
use http::HeaderValue;
use http::Method;
use serde_json::Value;
use serde_json::json;
use tokio::time::sleep;
use tokio::time::timeout;
use tower::BoxError;
use tower::ServiceExt;

use crate::integration::common::graph_os_enabled;
use crate::integration::response_cache::namespace;

const REDIS_URL: &str = "redis://127.0.0.1:6379";

macro_rules! check_cache_key {
    ($cache_key: expr, $client: expr) => {
        let mut record: Option<String> = None;
        // Retry a few times because insert is asynchronous
        for _ in 0..10 {
            match timeout(Duration::from_secs(5), $client.get($cache_key.clone())).await {
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

        let data = record
            .and_then(|record| serde_json::from_str::<Value>(&record).ok())
            .and_then(|record| record.as_object().cloned());
        insta::assert_json_snapshot!(data, {".cache_control.created" => 0});
    };
}

#[tokio::test(flavor = "multi_thread")]
async fn entity_cache_basic() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let namespace = namespace();

    let client = Builder::from_config(Config::from_url(REDIS_URL).unwrap()).build()?;
    client.init().await.unwrap();

    let subgraphs = serde_json::json!({
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
            "entities": [
                {
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
                }
            ],
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
                            "urls": [REDIS_URL],
                            "namespace": namespace,
                            "pool_size": 3,
                            "required_to_start": false
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
        .method(Method::POST)
        .header("apollo-cache-debugging", "true")
        .build()?;

    let response = supergraph
        .oneshot(request)
        .await?
        .next_response()
        .await
        .unwrap();
    dbg!(&response);
    insta::assert_json_snapshot!(response, {
        ".extensions.apolloCacheDebugging.data[].cacheControl.created" => 0
    });

    let make_pck = |key| format!("{namespace}:{key}");
    let cache_key = make_pck(
        "version:1.0:subgraph:products:type:Query:hash:bf44683f0c222652b509d6efb8f324610c8671181de540a96a5016bd71daa7cc:data:070af9367f9025bd796a1b7e0cd1335246f658aa4857c3a4d6284673b7d07fa6",
    );
    check_cache_key!(cache_key, &client);

    let cache_key = make_pck(
        "version:1.0:subgraph:reviews:type:Product:entity:cf4952a1e511b1bf2561a6193b4cdfc95f265a79e5cae4fd3e46fd9e75bc512f:representation::hash:06a24c8b3861c95f53d224071ee9627ee81b4826d23bc3de69bdc0031edde6ed:data:070af9367f9025bd796a1b7e0cd1335246f658aa4857c3a4d6284673b7d07fa6",
    );
    check_cache_key!(cache_key, &client);

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
                            "urls": [REDIS_URL],
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
        .method(Method::POST)
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

    let cache_key = make_pck(
        "version:1.0:subgraph:reviews:type:Product:entity:cf4952a1e511b1bf2561a6193b4cdfc95f265a79e5cae4fd3e46fd9e75bc512f:representation::hash:06a24c8b3861c95f53d224071ee9627ee81b4826d23bc3de69bdc0031edde6ed:data:070af9367f9025bd796a1b7e0cd1335246f658aa4857c3a4d6284673b7d07fa6",
    );
    check_cache_key!(cache_key, &client);

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
                            "urls": [REDIS_URL],
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
        .body(from_bytes(
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
        3u64
    );
    assert!(response_status.is_success());

    // This should be in error because we invalidated this entity
    let cache_key = make_pck(
        "version:1.0:subgraph:reviews:type:Product:entity:cf4952a1e511b1bf2561a6193b4cdfc95f265a79e5cae4fd3e46fd9e75bc512f:representation::hash:06a24c8b3861c95f53d224071ee9627ee81b4826d23bc3de69bdc0031edde6ed:data:070af9367f9025bd796a1b7e0cd1335246f658aa4857c3a4d6284673b7d07fa6",
    );
    check_cache_key!(cache_key, &client);

    // This entry should still be in redis because we didn't invalidate this entry
    let cache_key = make_pck(
        "version:1.0:subgraph:products:type:Query:hash:bf44683f0c222652b509d6efb8f324610c8671181de540a96a5016bd71daa7cc:data:070af9367f9025bd796a1b7e0cd1335246f658aa4857c3a4d6284673b7d07fa6",
    );
    check_cache_key!(cache_key, &client);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn entity_cache_with_nested_field_set() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let namespace = namespace();
    let schema = include_str!("../../src/testdata/supergraph_nested_fields.graphql");

    let client = Builder::from_config(Config::from_url(REDIS_URL).unwrap()).build()?;
    client.init().await.unwrap();

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
                            "urls": [REDIS_URL],
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
        .method(Method::POST)
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

    let make_pck = |key| format!("{namespace}:{key}");

    let cache_key = make_pck(
        "version:1.0:subgraph:products:type:Query:hash:f4f41cfa309494d41648c3a3c398c61cb00197696102199454a25a0dcdd2f592:data:070af9367f9025bd796a1b7e0cd1335246f658aa4857c3a4d6284673b7d07fa6",
    );
    check_cache_key!(cache_key, &client);

    let cache_key = make_pck(
        "version:1.0:subgraph:users:type:User:entity:b41dfad85edaabac7bb681098e9b23e21b3b8b9b8b1849babbd5a1300af64b43:representation:68fd4df7c06fd234bd0feb24e3300abcc06136ea8a9dd7533b7378f5fce7cfc4:hash:460b70e698b8c9d8496b0567e0f0848b9f7fef36e841a8a0b0771891150c35e5:data:070af9367f9025bd796a1b7e0cd1335246f658aa4857c3a4d6284673b7d07fa6",
    );
    check_cache_key!(cache_key, &client);

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
                            "urls": [REDIS_URL],
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
        .method(Method::POST)
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

    let cache_key = make_pck(
        "version:1.0:subgraph:users:type:User:entity:b41dfad85edaabac7bb681098e9b23e21b3b8b9b8b1849babbd5a1300af64b43:representation:68fd4df7c06fd234bd0feb24e3300abcc06136ea8a9dd7533b7378f5fce7cfc4:hash:460b70e698b8c9d8496b0567e0f0848b9f7fef36e841a8a0b0771891150c35e5:data:070af9367f9025bd796a1b7e0cd1335246f658aa4857c3a4d6284673b7d07fa6",
    );
    check_cache_key!(cache_key, &client);

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
                            "urls": [REDIS_URL],
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
        .body(from_bytes(
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
    let cache_key = make_pck(
        "version:1.0:subgraph:users:type:User:entity:b41dfad85edaabac7bb681098e9b23e21b3b8b9b8b1849babbd5a1300af64b43:representation:68fd4df7c06fd234bd0feb24e3300abcc06136ea8a9dd7533b7378f5fce7cfc4:hash:460b70e698b8c9d8496b0567e0f0848b9f7fef36e841a8a0b0771891150c35e5:data:070af9367f9025bd796a1b7e0cd1335246f658aa4857c3a4d6284673b7d07fa6",
    );
    check_cache_key!(cache_key, &client);

    // This entry should still be in redis because we didn't invalidate this entry
    let cache_key = make_pck(
        "version:1.0:subgraph:products:type:Query:hash:f4f41cfa309494d41648c3a3c398c61cb00197696102199454a25a0dcdd2f592:data:070af9367f9025bd796a1b7e0cd1335246f658aa4857c3a4d6284673b7d07fa6",
    );
    check_cache_key!(cache_key, &client);

    Ok(())
}
