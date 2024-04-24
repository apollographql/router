#[cfg(all(target_os = "linux", target_arch = "x86_64", test))]
mod test {
    use apollo_router::plugin::test::MockSubgraph;
    use apollo_router::services::execution::QueryPlan;
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
    use serde::Deserialize;
    use serde::Serialize;
    use serde_json::json;
    use serde_json::Value;
    use tower::BoxError;
    use tower::ServiceExt;

    #[tokio::test(flavor = "multi_thread")]
    async fn query_planner() -> Result<(), BoxError> {
        // If this test fails and the cache key format changed you'll need to update the key here.
        // 1. Force this test to run locally by removing the cfg() line at the top of this file.
        // 2. run `docker compose up -d` and connect to the redis container by running `docker-compose exec redis /bin/bash`.
        // 3. Run the `redis-cli` command from the shell and start the redis `monitor` command.
        // 4. Run this test and yank the updated cache key from the redis logs.
        let known_cache_key = "plan:v2.7.4:af1ee357bc75cfbbcc6adda41089a56e7d1d52f6d44c049739dde2c259314f58:3973e022e93220f9212c18d0d0c543ae7c309e46640da93a4a0314de999f5112:2bf7810d3a47b31d8a77ebb09cdc784a3f77306827dc55b06770030a858167c7";

        let config = RedisConfig::from_url("redis://127.0.0.1:6379")?;
        let client = RedisClient::new(config, None, None, None);
        let connection_task = client.connect();
        client.wait_for_connect().await?;

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
            }))?
            .schema(include_str!("../fixtures/supergraph.graphql"))
            .build_supergraph()
            .await?;

        let request = supergraph::Request::fake_builder()
            .query(r#"{ topProducts { name name2:name } }"#)
            .method(Method::POST)
            .build()?;

        let _ = supergraph.oneshot(request).await?.next_response().await;

        let s: String = match client.get(known_cache_key).await {
            Ok(s) => s,
            Err(e) => {
                println!("keys in Redis server:");
                let mut scan = client.scan("plan:*", None, Some(ScanType::String));
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
            }))?
            .schema(include_str!("../fixtures/supergraph.graphql"))
            .build_supergraph()
            .await?;

        let request = supergraph::Request::fake_builder()
            .query(r#"{ topProducts { name name2:name } }"#)
            .method(Method::POST)
            .build()?;
        let _ = supergraph.oneshot(request).await?.next_response().await;
        let new_exp: i64 = client
            .custom_raw(cmd!("EXPIRETIME"), vec![known_cache_key.to_string()])
            .await
            .and_then(|frame| frame.try_into())
            .and_then(|value: RedisValue| value.convert())
            .unwrap();

        assert!(exp < new_exp);

        client.quit().await?;
        // calling quit ends the connection and event listener tasks
        let _ = connection_task.await;
        Ok(())
    }

    #[derive(Deserialize, Serialize)]

    struct QueryPlannerContent {
        plan: QueryPlan,
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apq() -> Result<(), BoxError> {
        let config = RedisConfig::from_url("redis://127.0.0.1:6379")?;
        let client = RedisClient::new(config, None, None, None);
        let connection_task = client.connect();
        client.wait_for_connect().await?;

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
            .configuration_json(config.clone())?
            .schema(include_str!("../fixtures/supergraph.graphql"))
            .build_router()
            .await?;

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
            .build()?
            .try_into()
            .unwrap();

        let res = router
            .clone()
            .oneshot(request)
            .await?
            .into_graphql_response_stream()
            .await
            .next()
            .await
            .unwrap()?;
        assert_eq!(
            res.errors.first().unwrap().message,
            "PersistedQueryNotFound"
        );
        let r: Option<String> = client.get(&format!("apq:{query_hash}")).await.unwrap();
        assert!(r.is_none());

        // Now we register the query
        // it should set a value in Redis
        let request: router::Request = supergraph::Request::fake_builder()
            .query(r#"{ topProducts { name name2:name } }"#)
            .extension("persistedQuery", persisted.clone())
            .method(Method::POST)
            .build()?
            .try_into()
            .unwrap();

        let res = router
            .clone()
            .oneshot(request)
            .await?
            .into_graphql_response_stream()
            .await
            .next()
            .await
            .unwrap()?;
        assert!(res.data.is_some());
        assert!(res.errors.is_empty());

        let s: Option<String> = client.get(&format!("apq:{query_hash}")).await.unwrap();
        insta::assert_display_snapshot!(s.unwrap());

        // we start a new router with the same config
        // it should have the same connection to Redis, but the in memory cache has been reset
        let router = apollo_router::TestHarness::builder()
            .with_subgraph_network_requests()
            .configuration_json(config.clone())?
            .schema(include_str!("../fixtures/supergraph.graphql"))
            .build_router()
            .await?;

        // a request with only the hash should succeed because it is stored in Redis
        let request: router::Request = supergraph::Request::fake_builder()
            .extension("persistedQuery", persisted.clone())
            .method(Method::POST)
            .build()?
            .try_into()
            .unwrap();

        let res = router
            .clone()
            .oneshot(request)
            .await?
            .into_graphql_response_stream()
            .await
            .next()
            .await
            .unwrap()?;
        assert!(res.data.is_some());
        assert!(res.errors.is_empty());

        client.quit().await?;
        // calling quit ends the connection and event listener tasks
        let _ = connection_task.await;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn entity_cache() -> Result<(), BoxError> {
        let config = RedisConfig::from_url("redis://127.0.0.1:6379")?;
        let client = RedisClient::new(config, None, None, None);
        let connection_task = client.connect();
        client.wait_for_connect().await?;

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
                    "redis": {
                        "urls": ["redis://127.0.0.1:6379"],
                        "ttl": "2s"
                    },
                    "enabled": false,
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
                },
                "include_subgraph_errors": {
                    "all": true
                }
            }))?
            .extra_plugin(subgraphs)
            .schema(include_str!("../fixtures/supergraph-auth.graphql"))
            .build_supergraph()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .query(r#"{ topProducts { name reviews { body } } }"#)
            .method(Method::POST)
            .build()?;

        let response = supergraph
            .oneshot(request)
            .await?
            .next_response()
            .await
            .unwrap();
        insta::assert_json_snapshot!(response);

        let s:String = client
          .get("subgraph:products:Query:07bd08ba4eb8b85451edd3b3aae3c3ad3dc0892d86deedde6e6d53f6415f807f:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c")
          .await
          .unwrap();
        let v: Value = serde_json::from_str(&s).unwrap();
        insta::assert_json_snapshot!(v.as_object().unwrap().get("data").unwrap());

        let s: String = client.get("subgraph:reviews:Product:4911f7a9dbad8a47b8900d65547503a2f3c0359f65c0bc5652ad9b9843281f66:826d5cf03645266e30655c7475530e2d40e0d5978595b0ab16318b1ce87c0fe1:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c").await.unwrap();
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
                    "redis": {
                        "urls": ["redis://127.0.0.1:6379"],
                        "ttl": "2s"
                    },
                    "enabled": false,
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
                },
                "include_subgraph_errors": {
                    "all": true
                }
            }))?
            .extra_plugin(subgraphs)
            .schema(include_str!("../fixtures/supergraph-auth.graphql"))
            .build_supergraph()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .query(r#"{ topProducts(first: 2) { name reviews { body } } }"#)
            .method(Method::POST)
            .build()?;

        let response = supergraph
            .oneshot(request)
            .await?
            .next_response()
            .await
            .unwrap();
        insta::assert_json_snapshot!(response);

        let s:String = client
        .get("subgraph:reviews:Product:d9a4cd73308dd13ca136390c10340823f94c335b9da198d2339c886c738abf0d:826d5cf03645266e30655c7475530e2d40e0d5978595b0ab16318b1ce87c0fe1:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c")
        .await
        .unwrap();
        let v: Value = serde_json::from_str(&s).unwrap();
        insta::assert_json_snapshot!(v.as_object().unwrap().get("data").unwrap());

        client.quit().await?;
        // calling quit ends the connection and event listener tasks
        let _ = connection_task.await;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn entity_cache_authorization() -> Result<(), BoxError> {
        let config = RedisConfig::from_url("redis://127.0.0.1:6379")?;
        let client = RedisClient::new(config, None, None, None);
        let connection_task = client.connect();
        client.wait_for_connect().await?;

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
                    "redis": {
                        "urls": ["redis://127.0.0.1:6379"],
                        "ttl": "2s"
                    },
                    "enabled": false,
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
                },
                "authorization": {
                    "preview_directives": {
                        "enabled": true
                    }
                },
                "include_subgraph_errors": {
                    "all": true
                }
            }))?
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
            .query(
                r#"{ me { id name } topProducts { name reviews { body author { username } } } }"#,
            )
            .context(context)
            .method(Method::POST)
            .build()?;

        let response = supergraph
            .clone()
            .oneshot(request)
            .await?
            .next_response()
            .await
            .unwrap();
        insta::assert_json_snapshot!(response);

        let s:String = client
          .get("subgraph:products:Query:07bd08ba4eb8b85451edd3b3aae3c3ad3dc0892d86deedde6e6d53f6415f807f:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c")
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
        .get("subgraph:reviews:Product:4911f7a9dbad8a47b8900d65547503a2f3c0359f65c0bc5652ad9b9843281f66:826d5cf03645266e30655c7475530e2d40e0d5978595b0ab16318b1ce87c0fe1:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c")
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
            .query(
                r#"{ me { id name } topProducts { name reviews { body author { username } } } }"#,
            )
            .context(context)
            .method(Method::POST)
            .build()?;

        let response = supergraph
            .clone()
            .oneshot(request)
            .await?
            .next_response()
            .await
            .unwrap();
        insta::assert_json_snapshot!(response);

        let s:String = client
          .get("subgraph:reviews:Product:4911f7a9dbad8a47b8900d65547503a2f3c0359f65c0bc5652ad9b9843281f66:c75297b98da101021e30020db99a3a11c2f9ac2008de94ce410c47940162e304:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c")
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
            .query(
                r#"{ me { id name } topProducts { name reviews { body author { username } } } }"#,
            )
            .context(context)
            .method(Method::POST)
            .build()?;

        let response = supergraph
            .clone()
            .oneshot(request)
            .await?
            .next_response()
            .await
            .unwrap();
        insta::assert_json_snapshot!(response);

        client.quit().await?;
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
        assert_eq!(
            e.to_string(),
            "couldn't build Router service: IO Error: Os { code: 111, kind: ConnectionRefused, message: \"Connection refused\" }"
        );
    }
}
