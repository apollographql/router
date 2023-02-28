#[cfg(all(target_os = "linux", target_arch = "x86_64", test))]
mod test {
    use apollo_router::services::execution::QueryPlan;
    use apollo_router::services::router;
    use apollo_router::services::supergraph;
    use fred::prelude::ClientLike;
    use fred::prelude::KeysInterface;
    use fred::prelude::RedisClient;
    use fred::types::RedisConfig;
    use futures::StreamExt;
    use http::Method;
    use serde::Deserialize;
    use serde::Serialize;
    use serde_json::json;
    use tower::BoxError;
    use tower::ServiceExt;

    #[tokio::test(flavor = "multi_thread")]
    async fn query_planner() -> Result<(), BoxError> {
        let client = RedisClient::new(
            RedisConfig::from_url("redis://127.0.0.1:6379").unwrap(),
            None,
            None,
        );
        client
            .wait_for_connect()
            .await
            .expect("opening redis client");

        client
        .del::<(), &'static str>("plan\x005abb5fecf7df056396fb90fdf38d430b8c1fec55ec132fde878161608af18b76\x00{ topProducts { name name2:name } }\x00-")
          .await
          .unwrap();

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
          .get("plan\x005abb5fecf7df056396fb90fdf38d430b8c1fec55ec132fde878161608af18b76\x00{ topProducts { name name2:name } }\x00-")
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

        Ok(())
    }

    #[derive(Deserialize, Serialize)]

    struct QueryPlannerContent {
        plan: QueryPlan,
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apq() -> Result<(), BoxError> {
        let client = RedisClient::new(
            RedisConfig::from_url("redis://localhost:6379").unwrap(),
            None,
            None,
        );

        // spawn tasks that listen for connection close or reconnect events
        let mut error_rx = client.on_error();
        let mut reconnect_rx = client.on_reconnect();
        tokio::spawn(async move {
            while let Ok(error) = error_rx.recv().await {
                tracing::error!("Client disconnected with error: {:?}", error);
            }
        });
        tokio::spawn(async move {
            while reconnect_rx.recv().await.is_ok() {
                tracing::info!("Redis client reconnected.");
            }
        });

        println!("redis wait for connect");
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            client.wait_for_connect(),
        )
        .await
        .unwrap()
        .expect("opening redis client");
        println!("redis connected");

        let config = json!({
            "supergraph": {
                "apq": {
                    "experimental_cache": {
                        "in_memory": {
                            "limit": 2
                        },
                        "redis": {
                            "urls": ["redis://localhost:6379"]
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
            .del::<(), String>(format!("apq\x00{query_hash}"))
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

        Ok(())
    }
}
