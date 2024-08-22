use apollo_router::plugin::test::MockSubgraph;
use apollo_router::services::router;
use apollo_router::services::supergraph;
use apollo_router::Context;
use apollo_router::MockedSubgraphs;
use fred::cmd;
use fred::prelude::*;
use fred::types::ScanType;
use fred::types::Scanner;
use futures::StreamExt;
use http::header::CACHE_CONTROL;
use http::HeaderValue;
use http::Method;
use serde_json::json;
use serde_json::Value;
use tower::BoxError;
use tower::ServiceExt;

use crate::integration::common::graph_os_enabled;
use crate::integration::IntegrationTest;

#[tokio::test(flavor = "multi_thread")]
async fn query_planner_cache() -> Result<(), BoxError> {
    // If this test fails and the cache key format changed you'll need to update the key here.
    // 1. Force this test to run locally by removing the cfg() line at the top of this file.
    // 2. run `docker-compose up -d` and connect to the redis container by running `docker-compose exec redis /bin/bash`.
    // 3. Run the `redis-cli` command from the shell and start the redis `monitor` command.
    // 4. Run this test and yank the updated cache key from the redis logs.
    // If you have redis running locally, you can skip step 1 and 2, and proceed with steps 3-4.
    let known_cache_key = "plan:0:v2.8.3:16385ebef77959fcdc520ad507eb1f7f7df28f1d54a0569e3adabcb4cd00d7ce:3973e022e93220f9212c18d0d0c543ae7c309e46640da93a4a0314de999f5112:8ecc6cbc98bab2769e6666a72ba47a4ebd90e6f62256ddcbdc7f352a805e0fe6";

    let config = RedisConfig::from_url("redis://127.0.0.1:6379").unwrap();
    let client = RedisClient::new(config, None, None, None);
    let connection_task = client.connect();
    client.wait_for_connect().await.unwrap();

    client.del::<String, _>(known_cache_key).await.unwrap();

    let supergraph = apollo_router::TestHarness::builder()
        .with_subgraph_network_requests()
        .configuration_json(json!({
            "supergraph": {
                "query_planning": {
                    "cache": {
                        "in_memory": {
                            "limit": 2
                        },
                        "redis": {
                            "urls": ["redis://127.0.0.1:6379"],
                            "ttl": "10s"
                        }
                    }
                }
            }
        }))
        .unwrap()
        .schema(include_str!("../fixtures/supergraph.graphql"))
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(r#"{ topProducts { name name2:name } }"#)
        .method(Method::POST)
        .build()
        .unwrap();

    let _ = supergraph
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await;

    let s: String = match client.get(known_cache_key).await {
        Ok(s) => s,
        Err(e) => {
            println!("keys in Redis server:");
            let mut scan = client.scan("plan:*", Some(u32::MAX), Some(ScanType::String));
            while let Some(key) = scan.next().await {
                let key = key.as_ref().unwrap().results();
                println!("\t{key:?}");
            }
            panic!("key {known_cache_key} not found: {e}\nIf you see this error, make sure the federation version you use matches the redis key.");
        }
    };
    let exp: i64 = client
        .custom_raw(cmd!("EXPIRETIME"), vec![known_cache_key.to_string()])
        .await
        .and_then(|frame| frame.try_into())
        .and_then(|value: RedisValue| value.convert())
        .unwrap();
    let query_plan_res: serde_json::Value = serde_json::from_str(&s).unwrap();
    // ignore the usage reporting field for which the order of elements in `referenced_fields_by_type` can change
    let query_plan = query_plan_res
        .as_object()
        .unwrap()
        .get("Ok")
        .unwrap()
        .get("Plan")
        .unwrap()
        .get("plan")
        .unwrap()
        .get("root");

    insta::assert_json_snapshot!(query_plan);

    // test expiration refresh
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let supergraph = apollo_router::TestHarness::builder()
        .with_subgraph_network_requests()
        .configuration_json(json!({
            "supergraph": {
                "query_planning": {
                    "cache": {
                        "in_memory": {
                            "limit": 2
                        },
                        "redis": {
                            "urls": ["redis://127.0.0.1:6379"],
                            "ttl": "10s"
                        }
                    }
                }
            }
        }))
        .unwrap()
        .schema(include_str!("../fixtures/supergraph.graphql"))
        .build_supergraph()
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(r#"{ topProducts { name name2:name } }"#)
        .method(Method::POST)
        .build()
        .unwrap();
    let _ = supergraph
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await;
    let new_exp: i64 = client
        .custom_raw(cmd!("EXPIRETIME"), vec![known_cache_key.to_string()])
        .await
        .and_then(|frame| frame.try_into())
        .and_then(|value: RedisValue| value.convert())
        .unwrap();

    assert!(exp < new_exp);

    client.quit().await.unwrap();
    // calling quit ends the connection and event listener tasks
    let _ = connection_task.await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn apq() -> Result<(), BoxError> {
    let config = RedisConfig::from_url("redis://127.0.0.1:6379").unwrap();
    let client = RedisClient::new(config, None, None, None);
    let connection_task = client.connect();
    client.wait_for_connect().await.unwrap();

    let config = json!({
        "apq": {
            "router": {
                "cache": {
                    "in_memory": {
                        "limit": 2
                    },
                    "redis": {
                        "urls": ["redis://127.0.0.1:6379"],
                        "ttl": "10s"
                    }
                }
            }
        }
    });

    let router = apollo_router::TestHarness::builder()
        .with_subgraph_network_requests()
        .configuration_json(config.clone())
        .unwrap()
        .schema(include_str!("../fixtures/supergraph.graphql"))
        .build_router()
        .await
        .unwrap();

    let query_hash = "4c45433039407593557f8a982dafd316a66ec03f0e1ed5fa1b7ef8060d76e8ec";

    client
        .del::<String, _>(&format!("apq:{query_hash}"))
        .await
        .unwrap();

    let persisted = json!({
        "version" : 1,
        "sha256Hash" : query_hash
    });

    // an APQ should fail if we do not know about the hash
    // it should not set a value in Redis
    let request: router::Request = supergraph::Request::fake_builder()
        .extension("persistedQuery", persisted.clone())
        .method(Method::POST)
        .build()
        .unwrap()
        .try_into()
        .unwrap();

    let res = router
        .clone()
        .oneshot(request)
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await
        .next()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        res.errors.first().unwrap().message,
        "PersistedQueryNotFound"
    );
    let r: Option<String> = client.get(format!("apq:{query_hash}")).await.unwrap();
    assert!(r.is_none());

    // Now we register the query
    // it should set a value in Redis
    let request: router::Request = supergraph::Request::fake_builder()
        .query(r#"{ topProducts { name name2:name } }"#)
        .extension("persistedQuery", persisted.clone())
        .method(Method::POST)
        .build()
        .unwrap()
        .try_into()
        .unwrap();

    let res = router
        .clone()
        .oneshot(request)
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await
        .next()
        .await
        .unwrap()
        .unwrap();
    assert!(res.data.is_some());
    assert!(res.errors.is_empty());

    let s: Option<String> = client.get(format!("apq:{query_hash}")).await.unwrap();
    insta::assert_snapshot!(s.unwrap());

    // we start a new router with the same config
    // it should have the same connection to Redis, but the in memory cache has been reset
    let router = apollo_router::TestHarness::builder()
        .with_subgraph_network_requests()
        .configuration_json(config.clone())
        .unwrap()
        .schema(include_str!("../fixtures/supergraph.graphql"))
        .build_router()
        .await
        .unwrap();

    // a request with only the hash should succeed because it is stored in Redis
    let request: router::Request = supergraph::Request::fake_builder()
        .extension("persistedQuery", persisted.clone())
        .method(Method::POST)
        .build()
        .unwrap()
        .try_into()
        .unwrap();

    let res = router
        .clone()
        .oneshot(request)
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await
        .next()
        .await
        .unwrap()
        .unwrap();
    assert!(res.data.is_some());
    assert!(res.errors.is_empty());

    client.quit().await.unwrap();
    // calling quit ends the connection and event listener tasks
    let _ = connection_task.await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn entity_cache() -> Result<(), BoxError> {
    let config = RedisConfig::from_url("redis://127.0.0.1:6379").unwrap();
    let client = RedisClient::new(config, None, None, None);
    let connection_task = client.connect();
    client.wait_for_connect().await.unwrap();

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
            "preview_entity_cache": {
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

    let s:String = client
          .get("version:1.0:subgraph:products:type:Query:hash:0df945dc1bc08f7fc02e8905b4c72aa9112f29bb7a214e4a38d199f0aa635b48:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c")
          .await
          .unwrap();
    let v: Value = serde_json::from_str(&s).unwrap();
    insta::assert_json_snapshot!(v.as_object().unwrap().get("data").unwrap());

    let s: String = client.get("version:1.0:subgraph:reviews:type:Product:entity:4911f7a9dbad8a47b8900d65547503a2f3c0359f65c0bc5652ad9b9843281f66:hash:1de543dab57fde0f00247922ccc4f76d4c916ae26a89dd83cd1a62300d0cda20:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c").await.unwrap();
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
            "preview_entity_cache": {
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
            }
        }))
        .unwrap()
        .extra_plugin(subgraphs)
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
        .get("version:1.0:subgraph:reviews:type:Product:entity:d9a4cd73308dd13ca136390c10340823f94c335b9da198d2339c886c738abf0d:hash:1de543dab57fde0f00247922ccc4f76d4c916ae26a89dd83cd1a62300d0cda20:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c")
        .await
        .unwrap();
    let v: Value = serde_json::from_str(&s).unwrap();
    insta::assert_json_snapshot!(v.as_object().unwrap().get("data").unwrap());

    client.quit().await.unwrap();
    // calling quit ends the connection and event listener tasks
    let _ = connection_task.await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn entity_cache_authorization() -> Result<(), BoxError> {
    let config = RedisConfig::from_url("redis://127.0.0.1:6379").unwrap();
    let client = RedisClient::new(config, None, None, None);
    let connection_task = client.connect();
    client.wait_for_connect().await.unwrap();

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
            "preview_entity_cache": {
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
            "apollo_authorization::scopes::required",
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
          .get("version:1.0:subgraph:products:type:Query:hash:0df945dc1bc08f7fc02e8905b4c72aa9112f29bb7a214e4a38d199f0aa635b48:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c")
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
        .get("version:1.0:subgraph:reviews:type:Product:entity:4911f7a9dbad8a47b8900d65547503a2f3c0359f65c0bc5652ad9b9843281f66:hash:1de543dab57fde0f00247922ccc4f76d4c916ae26a89dd83cd1a62300d0cda20:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c")
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
            "apollo_authorization::scopes::required",
            json! {["profile", "read:user", "read:name"]},
        )
        .unwrap();
    context
        .insert(
            "apollo_authentication::JWT::claims",
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
          .get("version:1.0:subgraph:reviews:type:Product:entity:4911f7a9dbad8a47b8900d65547503a2f3c0359f65c0bc5652ad9b9843281f66:hash:3b6ef3c8fd34c469d59f513942c5f4c8f91135e828712de2024e2cd4613c50ae:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c")
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

    let context = Context::new();
    context
        .insert(
            "apollo_authorization::scopes::required",
            json! {["profile", "read:user", "read:name"]},
        )
        .unwrap();
    context
        .insert(
            "apollo_authentication::JWT::claims",
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

#[tokio::test(flavor = "multi_thread")]
async fn connection_failure_blocks_startup() {
    let _ = apollo_router::TestHarness::builder()
        .with_subgraph_network_requests()
        .configuration_json(json!({
            "supergraph": {
                "query_planning": {
                    "cache": {
                        "in_memory": {
                            "limit": 2
                        },
                        "redis": {
                            // invalid port
                            "urls": ["redis://127.0.0.1:6378"]
                        }
                    }
                }
            }
        }))
        .unwrap()
        .schema(include_str!("../fixtures/supergraph.graphql"))
        .build_supergraph()
        .await
        .unwrap();

    let e = apollo_router::TestHarness::builder()
        .with_subgraph_network_requests()
        .configuration_json(json!({
            "supergraph": {
                "query_planning": {
                    "cache": {
                        "in_memory": {
                            "limit": 2
                        },
                        "redis": {
                            // invalid port
                            "urls": ["redis://127.0.0.1:6378"],
                            "required_to_start": true
                        }
                    }
                }
            }
        }))
        .unwrap()
        .schema(include_str!("../fixtures/supergraph.graphql"))
        .build_supergraph()
        .await
        .unwrap_err();
    //OSX has a different error code for connection refused
    let e = e.to_string().replace("61", "111"); //
    assert_eq!(
            e,
            "couldn't build Router service: IO Error: Os { code: 111, kind: ConnectionRefused, message: \"Connection refused\" }"
        );
}

#[tokio::test(flavor = "multi_thread")]
async fn query_planner_redis_update_query_fragments() {
    test_redis_query_plan_config_update(
        include_str!("fixtures/query_planner_redis_config_update_query_fragments.router.yaml"),
        "plan:0:v2.8.3:a9e605fa09adc5a4b824e690b4de6f160d47d84ede5956b58a7d300cca1f7204:3973e022e93220f9212c18d0d0c543ae7c309e46640da93a4a0314de999f5112:cda2b4e476fdce9c4c435627b26cedd177cfbe04ab335fc3e3d895c0d79d965e",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "extraction of subgraphs from supergraph is not yet implemented"]
async fn query_planner_redis_update_planner_mode() {
    test_redis_query_plan_config_update(
        include_str!("fixtures/query_planner_redis_config_update_query_planner_mode.router.yaml"),
        "",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn query_planner_redis_update_introspection() {
    test_redis_query_plan_config_update(
        include_str!("fixtures/query_planner_redis_config_update_introspection.router.yaml"),
        "plan:0:v2.8.3:a9e605fa09adc5a4b824e690b4de6f160d47d84ede5956b58a7d300cca1f7204:3973e022e93220f9212c18d0d0c543ae7c309e46640da93a4a0314de999f5112:259dd917e4de09b5469629849b91e8ffdfbed2587041fad68b5963369bb13283",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn query_planner_redis_update_defer() {
    test_redis_query_plan_config_update(
        include_str!("fixtures/query_planner_redis_config_update_defer.router.yaml"),
        "plan:0:v2.8.3:a9e605fa09adc5a4b824e690b4de6f160d47d84ede5956b58a7d300cca1f7204:3973e022e93220f9212c18d0d0c543ae7c309e46640da93a4a0314de999f5112:e4376fe032160ce16399e520c6e815da6cb5cf4dc94a06175b86b64a9bf80201",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn query_planner_redis_update_type_conditional_fetching() {
    test_redis_query_plan_config_update(
        include_str!(
            "fixtures/query_planner_redis_config_update_type_conditional_fetching.router.yaml"
        ),
        "plan:0:v2.8.3:a9e605fa09adc5a4b824e690b4de6f160d47d84ede5956b58a7d300cca1f7204:3973e022e93220f9212c18d0d0c543ae7c309e46640da93a4a0314de999f5112:83d899fcb42d2202c39fc8350289b8247021da00ecf3d844553c190c49410507",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn query_planner_redis_update_reuse_query_fragments() {
    test_redis_query_plan_config_update(
        include_str!(
            "fixtures/query_planner_redis_config_update_reuse_query_fragments.router.yaml"
        ),
        "plan:0:v2.8.3:a9e605fa09adc5a4b824e690b4de6f160d47d84ede5956b58a7d300cca1f7204:3973e022e93220f9212c18d0d0c543ae7c309e46640da93a4a0314de999f5112:d48f92f892bd67071694c0538a7e657ff8e0c52e1718f475190c17b503e9e8c3",
    )
    .await;
}

async fn test_redis_query_plan_config_update(updated_config: &str, new_cache_key: &str) {
    if !graph_os_enabled() {
        return;
    }
    // This test shows that the redis key changes when the query planner config changes.
    // The test starts a router with a specific config, executes a query, and checks the redis cache key.
    // Then it updates the config, executes the query again, and checks the redis cache key.
    let mut router = IntegrationTest::builder()
        .config(include_str!(
            "fixtures/query_planner_redis_config_update.router.yaml"
        ))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.clear_redis_cache().await;

    let starting_key = "plan:0:v2.8.3:a9e605fa09adc5a4b824e690b4de6f160d47d84ede5956b58a7d300cca1f7204:3973e022e93220f9212c18d0d0c543ae7c309e46640da93a4a0314de999f5112:0966f1528d47cee30b6140a164be16148dd360ee10b87744991e9d35af8e8a27";
    router.execute_default_query().await;
    router.assert_redis_cache_contains(starting_key, None).await;
    router.update_config(updated_config).await;
    router.assert_reloaded().await;
    router.execute_default_query().await;
    router
        .assert_redis_cache_contains(new_cache_key, Some(starting_key))
        .await;
}
