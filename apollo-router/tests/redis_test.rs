#[cfg(all(target_os = "linux", target_arch = "x86_64", test))]
mod test {
    use apollo_router::plugin::test::MockSubgraph;
    use apollo_router::services::execution::QueryPlan;
    use apollo_router::services::router;
    use apollo_router::services::supergraph;
    use apollo_router::MockedSubgraphs;
    use fred::prelude::*;
    use futures::StreamExt;
    use http::Method;
    use serde::Deserialize;
    use serde::Serialize;
    use serde_json::json;
    use serde_json::Value;
    use tower::BoxError;
    use tower::ServiceExt;

    #[tokio::test(flavor = "multi_thread")]
    async fn query_planner() -> Result<(), BoxError> {
        let config = RedisConfig::from_url("redis://127.0.0.1:6379")?;
        let client = RedisClient::new(config, None, None);
        let connection_task = client.connect();
        client.wait_for_connect().await?;

        client.del::<String, _>("plan.5abb5fecf7df056396fb90fdf38d430b8c1fec55ec132fde878161608af18b76.4c45433039407593557f8a982dafd316a66ec03f0e1ed5fa1b7ef8060d76e8ec.3973e022e93220f9212c18d0d0c543ae7c309e46640da93a4a0314de999f5112.4f918cb09d5956bea87fe8addb4db3bd16de2cdf935e899cf252cac5528090e4").await.unwrap();

        let supergraph = apollo_router::TestHarness::builder()
            .with_subgraph_network_requests()
            .configuration_json(json!({
                "supergraph": {
                    "query_planning": {
                        "experimental_cache": {
                            "in_memory": {
                                "limit": 2
                            },
                            "redis": {
                                "urls": ["redis://127.0.0.1:6379"]
                            }
                        }
                    }
                }
            }))?
            .schema(include_str!("fixtures/supergraph.graphql"))
            .build_supergraph()
            .await?;

        let request = supergraph::Request::fake_builder()
            .query(r#"{ topProducts { name name2:name } }"#)
            .method(Method::POST)
            .build()?;

        let _ = supergraph.oneshot(request).await?.next_response().await;

        let s:String = client
          .get("plan.5abb5fecf7df056396fb90fdf38d430b8c1fec55ec132fde878161608af18b76.4c45433039407593557f8a982dafd316a66ec03f0e1ed5fa1b7ef8060d76e8ec.3973e022e93220f9212c18d0d0c543ae7c309e46640da93a4a0314de999f5112.4f918cb09d5956bea87fe8addb4db3bd16de2cdf935e899cf252cac5528090e4")
          .await
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
        let client = RedisClient::new(config, None, None);
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
                            "urls": ["redis://127.0.0.1:6379"]
                        }
                    }
                }
            }
        });

        let router = apollo_router::TestHarness::builder()
            .with_subgraph_network_requests()
            .configuration_json(config.clone())?
            .schema(include_str!("fixtures/supergraph.graphql"))
            .build_router()
            .await?;

        let query_hash = "4c45433039407593557f8a982dafd316a66ec03f0e1ed5fa1b7ef8060d76e8ec";

        client
            .del::<String, _>(&format!("apq\x00{query_hash}"))
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
        assert_eq!(res.errors.get(0).unwrap().message, "PersistedQueryNotFound");

        let r: Option<String> = client.get(&format!("apq\x00{query_hash}")).await.unwrap();
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

        let s: Option<String> = client.get(&format!("apq\x00{query_hash}")).await.unwrap();
        insta::assert_display_snapshot!(s.unwrap());

        // we start a new router with the same config
        // it should have the same connection to Redis, but the in memory cache has been reset
        let router = apollo_router::TestHarness::builder()
            .with_subgraph_network_requests()
            .configuration_json(config.clone())?
            .schema(include_str!("fixtures/supergraph.graphql"))
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
        let client = RedisClient::new(config, None, None);
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
        ).build());

        let supergraph = apollo_router::TestHarness::builder()
            .with_subgraph_network_requests()
            .configuration_json(json!({
                "experimental_entity_cache": {
                    "redis": {
                        "urls": ["redis://127.0.0.1:6379"],
                        "ttl": "2s"
                    },
                    /*
                    Uncomment when the configuration PR is merged
                    "enabled": false,
                    "subgraphs": {
                        "products": {
                            "enabled": true,
                            "ttl": "60s"
                        },
                        "accounts": {
                            "enabled": true,
                            "ttl": "10s"
                        }
                    }*/
                },
                "include_subgraph_errors": {
                    "all": true
                }
            }))?
            .extra_plugin(subgraphs)
            .schema(include_str!("fixtures/supergraph.graphql"))
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
          .get("subgraph.products|dd9d6384692afb6cfe37246c44208055c55b0b497736a3021e4561e05c577848|d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c")
          .await
          .unwrap();
        let v: Value = serde_json::from_str(&s).unwrap();
        insta::assert_json_snapshot!(v.as_object().unwrap().get("data").unwrap());

        let s:String = client
        .get("subgraph.reviews|Product|4911f7a9dbad8a47b8900d65547503a2f3c0359f65c0bc5652ad9b9843281f66|e7f00d16bc83326839fe1a0374b63a79a3d164501eda57f931301125972474f1|d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c")
        .await
        .unwrap();
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
        ).build());

        let supergraph = apollo_router::TestHarness::builder()
            .with_subgraph_network_requests()
            .configuration_json(json!({
                "experimental_entity_cache": {
                    "redis": {
                        "urls": ["redis://127.0.0.1:6379"],
                        "ttl": "2s"
                    },
                    /*
                    Uncomment when the configuration PR is merged
                    "enabled": false,
                    "subgraphs": {
                        "products": {
                            "enabled": true,
                            "ttl": "60s"
                        },
                        "accounts": {
                            "enabled": true,
                            "ttl": "10s"
                        }
                    }*/
                },
                "include_subgraph_errors": {
                    "all": true
                }
            }))?
            .extra_plugin(subgraphs)
            .schema(include_str!("fixtures/supergraph.graphql"))
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
        .get("subgraph.reviews|Product|d9a4cd73308dd13ca136390c10340823f94c335b9da198d2339c886c738abf0d|e7f00d16bc83326839fe1a0374b63a79a3d164501eda57f931301125972474f1|d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c")
        .await
        .unwrap();
        let v: Value = serde_json::from_str(&s).unwrap();
        insta::assert_json_snapshot!(v.as_object().unwrap().get("data").unwrap());

        client.quit().await?;
        // calling quit ends the connection and event listener tasks
        let _ = connection_task.await;
        Ok(())
    }
}
