// The redis cache keys in this file have to change whenever the following change:
// * the supergraph schema
// * federation version
//
// How to get the new cache key:
// If you have redis running locally, you can skip step 1 and proceed with steps 2-3.
// 1. run `docker-compose up -d` and connect to the redis container by running `docker-compose exec redis /bin/bash`.
// 2. Run the `redis-cli` command from the shell and start the redis `monitor` command.
// 3. Run the failing test and yank the updated cache key from the redis logs. It will be the key following `SET`, for example:
// ```bash
// 1724831727.472732 [0 127.0.0.1:56720] "SET"
// "plan:0:v2.8.5:70f115ebba5991355c17f4f56ba25bb093c519c4db49a30f3b10de279a4e3fa4:3973e022e93220f9212c18d0d0c543ae7c309e46640da93a4a0314de999f5112:4f9f0183101b2f249a364b98adadfda6e5e2001d1f2465c988428cf1ac0b545f"
// "{\"Ok\":{\"Plan\":{\"plan\":{\"usage_reporting\":{\"statsReportKey\":\"#
// -\\n{topProducts{name
// name}}\",\"referencedFieldsByType\":{\"Product\":{\"fieldNames\":[\"name\"],\"isInterface\":false},\"Query\":{\"fieldNames\":[\"topProducts\"],\"isInterface\":false}}},\"root\":{\"kind\":\"Fetch\",\"serviceName\":\"products\",\"variableUsages\":[],\"operation\":\"{topProducts{name
// name2:name}}\",\"operationName\":null,\"operationKind\":\"query\",\"id\":null,\"inputRewrites\":null,\"outputRewrites\":null,\"contextRewrites\":null,\"schemaAwareHash\":\"121b9859eba2d8fa6dde0a54b6e3781274cf69f7ffb0af912e92c01c6bfff6ca\",\"authorization\":{\"is_authenticated\":false,\"scopes\":[],\"policies\":[]}},\"formatted_query_plan\":\"QueryPlan
// {\\n  Fetch(service: \\\"products\\\") {\\n    {\\n      topProducts {\\n
// name\\n        name2: name\\n      }\\n    }\\n
// n  },\\n}\",\"query\":{\"string\":\"{\\n  topProducts {\\n    name\\n
// name2: name\\n
// }\\n}\\n\",\"fragments\":{\"map\":{}},\"operations\":[{\"name\":null,\"kind\":\"query\",\"type_name\":\"Query\",\"selection_set\":[{\"Field\":{\"name\":\"topProducts\",\"alias\":null,\"selection_set\":[{\"Field\":{\"name\":\"name\",\"alias\":null,\"selection_set\":null,\"field_type\":{\"Named\":\"String\"},\"include_skip\":{\"include\":\"Yes\",\"skip\":\"No\"}}},{\"Field\":{\"name\":\"name\",\"alias\":\"name2\",\"selection_set\":null,\"field_type\":{\"Named\":\"String\"},\"include_skip\":{\"include\":\"Yes\",\"skip\":\"No\"}}}],\"field_type\":{\"List\":{\"Named\":\"Product\"}},\"include_skip\":{\"include\":\"Yes\",\"skip\":\"No\"}}}],\"variables\":{}}],\"subselections\":{},\"unauthorized\":{\"paths\":[],\"errors\":{\"log\":true,\"response\":\"errors\"}},\"filtered_query\":null,\"defer_stats\":{\"has_defer\":false,\"has_unconditional_defer\":false,\"conditional_defer_variable_names\":[]},\"is_original\":true,\"schema_aware_hash\":[20,152,93,92,189,0,240,140,9,65,84,255,4,76,202,231,69,183,58,121,37,240,0,109,198,125,1,82,12,42,179,189]},\"query_metrics\":{\"depth\":2,\"height\":3,\"root_fields\":1,\"aliases\":1},\"estimated_size\":0}}}}"
// "EX" "10"
// ```

use apollo_router::Context;
use apollo_router::MockedSubgraphs;
use apollo_router::plugin::test::MockSubgraph;
use apollo_router::services::router;
use apollo_router::services::supergraph;
use fred::cmd;
use fred::prelude::*;
use fred::types::ScanType;
use fred::types::Scanner;
use futures::StreamExt;
use http::HeaderValue;
use http::Method;
use http::header::CACHE_CONTROL;
use serde_json::Value;
use serde_json::json;
use tower::BoxError;
use tower::ServiceExt;

use crate::integration::IntegrationTest;
use crate::integration::common::Query;
use crate::integration::common::graph_os_enabled;

#[tokio::test(flavor = "multi_thread")]
async fn query_planner_cache() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }
    // If this test fails and the cache key format changed you'll need to update the key here.
    // Look at the top of the file for instructions on getting the new cache key.
    let known_cache_key = &format!(
        "plan:router:{}:47939f0e964372951934fc662c9c2be675bc7116ec3e57029abe555284eb10a4:opname:3973e022e93220f9212c18d0d0c543ae7c309e46640da93a4a0314de999f5112:metadata:d9f7a00bc249cb51cfc8599f86b6dc5272967b37b1409dc4717f105b6939fe43",
        env!("CARGO_PKG_VERSION")
    );

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
            panic!(
                "key {known_cache_key} not found: {e}\nIf you see this error, make sure the federation version you use matches the redis key."
            );
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
    if !graph_os_enabled() {
        return Ok(());
    }

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
async fn entity_cache_basic() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

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

    client.quit().await.unwrap();
    // calling quit ends the connection and event listener tasks
    let _ = connection_task.await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn entity_cache_authorization() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        return Ok(());
    }

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

#[tokio::test(flavor = "multi_thread")]
async fn connection_failure_blocks_startup() {
    if !graph_os_enabled() {
        return;
    }

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
    // If this test fails and the cache key format changed you'll need to update
    // the key here.  Look at the top of the file for instructions on getting
    // the new cache key.
    //
    // You first need to follow the process and update the key in
    // `test_redis_query_plan_config_update`, and then update the key in this
    // test.
    //
    // This test requires graphos license, so make sure you have
    // "TEST_APOLLO_KEY" and "TEST_APOLLO_GRAPH_REF" env vars set, otherwise the
    // test just passes locally.
    test_redis_query_plan_config_update(
        // This configuration turns the fragment generation option *off*.
        include_str!("fixtures/query_planner_redis_config_update_query_fragments.router.yaml"),
        &format!(
            "plan:router:{}:14ece7260081620bb49f1f4934cf48510e5f16c3171181768bb46a5609d7dfb7:opname:3973e022e93220f9212c18d0d0c543ae7c309e46640da93a4a0314de999f5112:metadata:fb1a8e6e454ad6a1d0d48b24dc9c7c4dd6d9bf58b6fdaf43cd24eb77fbbb3a17",
            env!("CARGO_PKG_VERSION")
        ),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn query_planner_redis_update_defer() {
    // If this test fails and the cache key format changed you'll need to update
    // the key here.  Look at the top of the file for instructions on getting
    // the new cache key.
    //
    // You first need to follow the process and update the key in
    // `test_redis_query_plan_config_update`, and then update the key in this
    // test.
    //
    // This test requires graphos license, so make sure you have
    // "TEST_APOLLO_KEY" and "TEST_APOLLO_GRAPH_REF" env vars set, otherwise the
    // test just passes locally.
    test_redis_query_plan_config_update(
        include_str!("fixtures/query_planner_redis_config_update_defer.router.yaml"),
        &format!(
            "plan:router:{}:14ece7260081620bb49f1f4934cf48510e5f16c3171181768bb46a5609d7dfb7:opname:3973e022e93220f9212c18d0d0c543ae7c309e46640da93a4a0314de999f5112:metadata:dc062fcc9cfd9582402d1e8b1fa3ee336ea1804d833443869e0b3744996716a2",
            env!("CARGO_PKG_VERSION")
        ),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn query_planner_redis_update_type_conditional_fetching() {
    // If this test fails and the cache key format changed you'll need to update
    // the key here.  Look at the top of the file for instructions on getting
    // the new cache key.
    //
    // You first need to follow the process and update the key in
    // `test_redis_query_plan_config_update`, and then update the key in this
    // test.
    //
    // This test requires graphos license, so make sure you have
    // "TEST_APOLLO_KEY" and "TEST_APOLLO_GRAPH_REF" env vars set, otherwise the
    // test just passes locally.
    test_redis_query_plan_config_update(
        include_str!(
            "fixtures/query_planner_redis_config_update_type_conditional_fetching.router.yaml"
        ),
        &format!(
            "plan:router:{}:14ece7260081620bb49f1f4934cf48510e5f16c3171181768bb46a5609d7dfb7:opname:3973e022e93220f9212c18d0d0c543ae7c309e46640da93a4a0314de999f5112:metadata:bdc09980aa6ef28a67f5aeb8759763d8ac5a4fc43afa8c5a89f58cc998c48db3",
            env!("CARGO_PKG_VERSION")
        ),
    )
    .await;
}

async fn test_redis_query_plan_config_update(updated_config: &str, new_cache_key: &str) {
    if !graph_os_enabled() {
        return;
    }
    // This test shows that the redis key changes when the query planner config
    // changes.  The test starts a router with a specific config, executes a
    // query, and checks the redis cache key.  Then it updates the config,
    // executes the query again, and checks the redis cache key.
    let mut router = IntegrationTest::builder()
        .config(include_str!(
            "fixtures/query_planner_redis_config_update.router.yaml"
        ))
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.clear_redis_cache().await;

    // If the tests above are failing, this is the key that needs to be changed first.
    let starting_key = &format!(
        "plan:router:{}:14ece7260081620bb49f1f4934cf48510e5f16c3171181768bb46a5609d7dfb7:opname:3973e022e93220f9212c18d0d0c543ae7c309e46640da93a4a0314de999f5112:metadata:d9f7a00bc249cb51cfc8599f86b6dc5272967b37b1409dc4717f105b6939fe43",
        env!("CARGO_PKG_VERSION")
    );
    assert_ne!(
        starting_key, new_cache_key,
        "starting_key (cache key for the initial config) and new_cache_key (cache key with the updated config) should not be equal. This either means that the cache key is not being generated correctly, or that the test is not actually checking the updated key."
    );

    router
        .execute_query(Query::default().with_anonymous())
        .await;
    router.assert_redis_cache_contains(starting_key, None).await;
    router.update_config(updated_config).await;
    router.assert_reloaded().await;
    router
        .execute_query(Query::default().with_anonymous())
        .await;
    router
        .assert_redis_cache_contains(new_cache_key, Some(starting_key))
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_redis_connections_are_closed_on_router_reload() {
    if !graph_os_enabled() {
        return;
    }

    let router_config = include_str!("fixtures/redis_connection_closure.router.yaml");
    let mut router = IntegrationTest::builder()
        .config(router_config)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let expected_metric = format!(
        r#"apollo_router_cache_redis_connections{{kind="query planner",otel_scope_name="apollo/router",process_executable_name="router",service_name="unknown_service:router",service_version="{}"}} 4"#,
        std::env!("CARGO_PKG_VERSION")
    );
    router.assert_metrics_contains(&expected_metric, None).await;

    // check that reloading the schema yields the same number of redis connections
    let new_router_config = format!("{router_config}\ninclude_subgraph_errors:\n  all: true");
    router.update_config(&new_router_config).await;
    router.assert_reloaded().await;

    router.assert_metrics_contains(&expected_metric, None).await;
}
