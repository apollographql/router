use apollo_router::services::router::body::from_bytes;
use apollo_router::services::supergraph;
use http::HeaderValue;
use http::Method;
use serde_json::Value;
use serde_json::json;
use sqlx::Connection;
use sqlx::PgConnection;
use sqlx::prelude::FromRow;
use tower::BoxError;
use tower::ServiceExt;

use crate::integration::common::graph_os_enabled;

#[derive(FromRow)]
struct Record {
    data: String,
}

#[tokio::test(flavor = "multi_thread")]
async fn entity_cache_basic() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

    let mut conn = PgConnection::connect("postgres://127.0.0.1").await?;
    sqlx::migrate!().run(&mut conn).await.unwrap();
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
        .with_subgraph_network_requests()
        .configuration_json(json!({
            "preview_cache": {
                "enabled": true,
                "invalidation": {
                    "listen": "127.0.0.1:4000",
                    "path": "/invalidation"
                },
                "subgraph": {
                    "all": {
                        "enabled": false,
                        "postgres": {
                            "url": "postgres://127.0.0.1",
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

    let cache_key = "version:1.0:subgraph:products:type:Query:hash:30cf92cd31bc204de344385c8f6d90a53da6c9180d80e8f7979a5bc19cd96055:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c";
    let s: Record = sqlx::query_as!(
        Record,
        "SELECT data FROM cache WHERE cache_key = $1",
        cache_key
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    let v: Value = serde_json::from_str(&s.data).unwrap();
    insta::assert_json_snapshot!(v);

    let cache_key = "version:1.0:subgraph:reviews:type:Product:entity:72bafad9ffe61307806863b13856470e429e0cf332c99e5b735224fb0b1436f7:representation::hash:b9b8a9c94830cf56329ec2db7d7728881a6ba19cc1587710473e732e775a5870:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c";
    let s: Record = sqlx::query_as!(
        Record,
        "SELECT data FROM cache WHERE cache_key = $1",
        cache_key
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    let v: Value = serde_json::from_str(&s.data).unwrap();
    insta::assert_json_snapshot!(v);

    let supergraph = apollo_router::TestHarness::builder()
        .with_subgraph_network_requests()
        .configuration_json(json!({
            "preview_cache": {
                "enabled": true,
                "invalidation": {
                    "listen": "127.0.0.1:4000",
                    "path": "/invalidation"
                },
                "subgraph": {
                    "all": {
                        "enabled": false,
                        "postgres": {
                            "url": "postgres://127.0.0.1"
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

    let cache_key = "version:1.0:subgraph:reviews:type:Product:entity:080fc430afd3fb953a05525a6a00999226c34436466eff7ace1d33d004adaae3:representation::hash:b9b8a9c94830cf56329ec2db7d7728881a6ba19cc1587710473e732e775a5870:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c";
    let s: Record = sqlx::query_as!(
        Record,
        "SELECT data FROM cache WHERE cache_key = $1",
        cache_key
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    let v: Value = serde_json::from_str(&s.data).unwrap();
    insta::assert_json_snapshot!(v);

    const SECRET_SHARED_KEY: &str = "supersecret";
    let http_service = apollo_router::TestHarness::builder()
        .with_subgraph_network_requests()
        .configuration_json(json!({
            "preview_cache": {
                "enabled": true,
                "invalidation": {
                    "listen": "127.0.0.1:4000",
                    "path": "/invalidation"
                },
                "subgraph": {
                    "all": {
                        "enabled": true,
                        "postgres": {
                            "url": "postgres://127.0.0.1"
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
    let cache_key = "version:1.0:subgraph:reviews:type:Product:entity:080fc430afd3fb953a05525a6a00999226c34436466eff7ace1d33d004adaae3:representation::hash:b9b8a9c94830cf56329ec2db7d7728881a6ba19cc1587710473e732e775a5870:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c";
    assert!(
        sqlx::query_as!(
            Record,
            "SELECT data FROM cache WHERE cache_key = $1",
            cache_key
        )
        .fetch_one(&mut conn)
        .await
        .is_err()
    );
    // This entry should still be in redis because we didn't invalidate this entry
    let cache_key = "version:1.0:subgraph:products:type:Query:hash:30cf92cd31bc204de344385c8f6d90a53da6c9180d80e8f7979a5bc19cd96055:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c";
    assert!(
        sqlx::query_as!(
            Record,
            "SELECT data FROM cache WHERE cache_key = $1",
            cache_key
        )
        .fetch_one(&mut conn)
        .await
        .is_ok()
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn entity_cache_basic_with_directive() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    const SCHEMA: &str = include_str!("../fixtures/supergraph-auth_cache_key.graphql");

    let mut conn = PgConnection::connect("postgres://127.0.0.1").await?;
    sqlx::migrate!().run(&mut conn).await.unwrap();
    let subgraphs = serde_json::json!({
        "products": {
            "query": {"topProducts": [{
                "__typename": "Product",
                "upc": "4",
                "price": 5,
                "name": "chair"
            },
            {
                "__typename": "Product",
                "upc": "5",
                "price": 6,
                "name": "table"
            },
            {
                "__typename": "Product",
                "upc": "6",
                "price": 7,
                "name": "plate"
            }]},
            "headers": {"cache-control": "public"},
        },
        "reviews": {
            "entities": [{
                "__typename": "Product",
                "upc": "4",
                "reviews": [{
                    "__typename": "Review",
                    "body": "I can sit on it",
                }]
            },
            {
                "__typename": "Product",
                "upc": "5",
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
                "upc": "6",
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
        .with_subgraph_network_requests()
        .configuration_json(json!({
            "preview_cache": {
                "enabled": true,
                "invalidation": {
                    "listen": "127.0.0.1:4000",
                    "path": "/invalidation"
                },
                "subgraph": {
                    "all": {
                        "enabled": false,
                        "postgres": {
                            "url": "postgres://127.0.0.1",
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
        .schema(SCHEMA)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(r#"{ topProducts { upc name price reviews { body } } }"#)
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
    assert_eq!(
        serde_json::to_value(response).unwrap(),
        serde_json::json!({
          "data": {
            "topProducts": [
              {
                "upc": "4",
                "name": "chair",
                "price": 5,
                "reviews": [
                  {
                    "body": "I can sit on it"
                  }
                ]
              },
              {
                "upc": "5",
                "name": "table",
                "price": 6,
                "reviews": [
                  {
                    "body": "I can sit on it"
                  },
                  {
                    "body": "I can sit on it2"
                  }
                ]
              },
              {
                "upc": "6",
                "name": "plate",
                "price": 7,
                "reviews": [
                  {
                    "body": "I can sit on it"
                  },
                  {
                    "body": "I can sit on it2"
                  },
                  {
                    "body": "I can sit on it3"
                  }
                ]
              }
            ]
          }
        })
    );

    let cache_key = "version:1.0:subgraph:products:type:Query:hash:5f7033dc6948d07556d0f6a22e4df8279eebeeb596ef75ad51c1c3a45d0b2567:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c";
    let s: Record = sqlx::query_as!(
        Record,
        "SELECT data FROM cache WHERE cache_key = $1",
        cache_key
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    let v: Value = serde_json::from_str(&s.data).unwrap();
    assert_eq!(
        v,
        serde_json::json!({
              "topProducts": [
                {
                  "__typename": "Product",
                  "upc": "4",
                  "price": 5,
                  "name": "chair"
                },
                {
                  "__typename": "Product",
                  "upc": "5",
                  "price": 6,
                  "name": "table"
                },
                {
                  "__typename": "Product",
                  "upc": "6",
                  "price": 7,
                  "name": "plate"
                }
              ]
            }
        )
    );

    let cache_key = "version:1.0:subgraph:reviews:type:Product:entity:9a894d1be8b58b0269c0829ffa4f1d61bf9df2738437a5270731be6047bb7e58:representation::hash:b9b8a9c94830cf56329ec2db7d7728881a6ba19cc1587710473e732e775a5870:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c";
    let s: Record = sqlx::query_as!(
        Record,
        "SELECT data FROM cache WHERE cache_key = $1",
        cache_key
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    let v: Value = serde_json::from_str(&s.data).unwrap();
    assert_eq!(
        v,
        serde_json::json!({
          "reviews": [
            {
              "body": "I can sit on it"
            }
          ]
        })
    );

    let supergraph = apollo_router::TestHarness::builder()
        .with_subgraph_network_requests()
        .configuration_json(json!({
            "preview_cache": {
                "enabled": true,
                "invalidation": {
                    "listen": "127.0.0.1:4000",
                    "path": "/invalidation"
                },
                "subgraph": {
                    "all": {
                        "enabled": false,
                        "postgres": {
                            "url": "postgres://127.0.0.1"
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
        .schema(SCHEMA)
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(r#"{ topProducts(first: 2) { upc name price reviews { body } } }"#)
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
    assert_eq!(
        serde_json::to_value(response).unwrap(),
        serde_json::json!({
          "data": {
            "topProducts": [
              {
                "upc": "4",
                "price": 5,
                "name": "chair",
                "reviews": [
                  {
                    "body": "I can sit on it"
                  }
                ]
              },
              {
                "upc": "5",
                "price": 6,
                "name": "table",
                "reviews": [
                  {
                    "body": "I can sit on it"
                  },
                  {
                    "body": "I can sit on it2"
                  }
                ]
              },
              {
                "upc": "6",
                "price": 7,
                "name": "plate",
                "reviews": [
                  {
                    "body": "I can sit on it"
                  },
                  {
                    "body": "I can sit on it2"
                  },
                  {
                    "body": "I can sit on it3"
                  }
                ]
              }
            ]
          }
        })
    );

    let cache_key = "version:1.0:subgraph:reviews:type:Product:entity:20a3418db67808ee50198c3f1091742cd72f9391053f23299ecfca14c86007f2:representation::hash:b9b8a9c94830cf56329ec2db7d7728881a6ba19cc1587710473e732e775a5870:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c";
    let s: Record = sqlx::query_as!(
        Record,
        "SELECT data FROM cache WHERE cache_key = $1",
        cache_key
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    let v: Value = serde_json::from_str(&s.data).unwrap();
    assert_eq!(
        v,
        serde_json::json!({
              "reviews": [
                {
                  "body": "I can sit on it"
                },
                {
                  "body": "I can sit on it2"
                },
                {
                  "body": "I can sit on it3"
                }
              ]
            }
        )
    );

    const SECRET_SHARED_KEY: &str = "supersecret";
    let http_service = apollo_router::TestHarness::builder()
        .with_subgraph_network_requests()
        .configuration_json(json!({
            "preview_cache": {
                "enabled": true,
                "invalidation": {
                    "listen": "127.0.0.1:4000",
                    "path": "/invalidation"
                },
                "subgraph": {
                    "all": {
                        "enabled": true,
                        "postgres": {
                            "url": "postgres://127.0.0.1"
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
        .schema(SCHEMA)
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
                "subgraphs": ["reviews"],
                "kind": "cache_key",
                "cache_key": "product-4"
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

    assert!(response_status.is_success());
    assert_eq!(
        resp.as_object_mut()
            .unwrap()
            .get("count")
            .unwrap()
            .as_u64()
            .unwrap(),
        1u64
    );

    // This should be in error because we invalidated this entity
    let cache_key = "version:1.0:subgraph:reviews:type:Product:entity:9a894d1be8b58b0269c0829ffa4f1d61bf9df2738437a5270731be6047bb7e58:representation::hash:b9b8a9c94830cf56329ec2db7d7728881a6ba19cc1587710473e732e775a5870:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c";
    assert!(
        sqlx::query_as!(
            Record,
            "SELECT data FROM cache WHERE cache_key = $1",
            cache_key
        )
        .fetch_one(&mut conn)
        .await
        .is_err()
    );
    // This entry should still be in redis because we didn't invalidate this entry
    let cache_key = "version:1.0:subgraph:products:type:Query:hash:5f7033dc6948d07556d0f6a22e4df8279eebeeb596ef75ad51c1c3a45d0b2567:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c";
    assert!(
        sqlx::query_as!(
            Record,
            "SELECT data FROM cache WHERE cache_key = $1",
            cache_key
        )
        .fetch_one(&mut conn)
        .await
        .is_ok()
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn entity_cache_with_nested_field_set() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    let schema = include_str!("../../src/testdata/supergraph_nested_fields.graphql");

    let mut conn = PgConnection::connect("postgres://127.0.0.1").await?;
    sqlx::migrate!().run(&mut conn).await.unwrap();

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
            "preview_cache": {
                "enabled": true,
                "invalidation": {
                    "listen": "127.0.0.1:4000",
                    "path": "/invalidation"
                },
                "subgraph": {
                    "all": {
                        "enabled": true,
                        "postgres": {
                            "url": "postgres://127.0.0.1",
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

    let cache_key = "version:1.0:subgraph:products:type:Query:hash:bead1beb068f19990b462d5d7c17974a33b822f4b3439407a9f820ce1cb47de9:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c";
    let s: Record = sqlx::query_as!(
        Record,
        "SELECT data FROM cache WHERE cache_key = $1",
        cache_key
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    let v: Value = serde_json::from_str(&s.data).unwrap();
    insta::assert_json_snapshot!(v);

    let cache_key = "version:1.0:subgraph:users:type:User:entity:210e26346d676046faa9fb55d459273a43e5b5397a1a056f179a3521dc5643aa:representation:7cd02a08f4ea96f0affa123d5d3f56abca20e6014e060fe5594d210c00f64b27:hash:cfc5f467f767710804724ff6a05c3f63297328cd8283316adb25f5642e1439ad:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c";
    let s: Record = sqlx::query_as!(
        Record,
        "SELECT data FROM cache WHERE cache_key = $1",
        cache_key
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    let v: Value = serde_json::from_str(&s.data).unwrap();
    insta::assert_json_snapshot!(v);

    let supergraph = apollo_router::TestHarness::builder()
        .with_subgraph_network_requests()
        .configuration_json(json!({
            "preview_cache": {
                "enabled": true,
                "invalidation": {
                    "listen": "127.0.0.1:4000",
                    "path": "/invalidation"
                },
                "subgraph": {
                    "all": {
                        "enabled": false,
                        "postgres": {
                            "url": "postgres://127.0.0.1"
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

    let cache_key = "version:1.0:subgraph:users:type:User:entity:210e26346d676046faa9fb55d459273a43e5b5397a1a056f179a3521dc5643aa:representation:7cd02a08f4ea96f0affa123d5d3f56abca20e6014e060fe5594d210c00f64b27:hash:cfc5f467f767710804724ff6a05c3f63297328cd8283316adb25f5642e1439ad:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c";
    let s: Record = sqlx::query_as!(
        Record,
        "SELECT data FROM cache WHERE cache_key = $1",
        cache_key
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    let v: Value = serde_json::from_str(&s.data).unwrap();
    insta::assert_json_snapshot!(v);

    const SECRET_SHARED_KEY: &str = "supersecret";
    let http_service = apollo_router::TestHarness::builder()
        .with_subgraph_network_requests()
        .configuration_json(json!({
            "preview_cache": {
                "enabled": true,
                "invalidation": {
                    "listen": "127.0.0.1:4000",
                    "path": "/invalidation"
                },
                "subgraph": {
                    "all": {
                        "enabled": true,
                        "postgres": {
                            "url": "postgres://127.0.0.1"
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
    let cache_key = "version:1.0:subgraph:users:type:User:entity:210e26346d676046faa9fb55d459273a43e5b5397a1a056f179a3521dc5643aa:representation:7cd02a08f4ea96f0affa123d5d3f56abca20e6014e060fe5594d210c00f64b27:hash:cfc5f467f767710804724ff6a05c3f63297328cd8283316adb25f5642e1439ad:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c";
    assert!(
        sqlx::query_as!(
            Record,
            "SELECT data FROM cache WHERE cache_key = $1",
            cache_key
        )
        .fetch_one(&mut conn)
        .await
        .is_err()
    );

    // This entry should still be in redis because we didn't invalidate this entry
    let cache_key = "version:1.0:subgraph:products:type:Query:hash:bead1beb068f19990b462d5d7c17974a33b822f4b3439407a9f820ce1cb47de9:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c";
    assert!(
        sqlx::query_as!(
            Record,
            "SELECT data FROM cache WHERE cache_key = $1",
            cache_key
        )
        .fetch_one(&mut conn)
        .await
        .is_ok()
    );

    Ok(())
}
