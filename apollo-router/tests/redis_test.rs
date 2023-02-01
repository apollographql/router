#[cfg(all(target_os = "linux", target_arch = "x86_64", test))]
mod test {
    use apollo_router::services::execution::QueryPlan;
    use apollo_router::services::router;
    use apollo_router::services::supergraph;
    use futures::StreamExt;
    use http::Method;
    use redis::AsyncCommands;
    use redis::Client;
    use serde::Deserialize;
    use serde::Serialize;
    use serde_json::json;
    use tower::ServiceExt;

    #[tokio::test(flavor = "multi_thread")]
    async fn query_planner() {
        let client = Client::open("redis://127.0.0.1:6379").expect("opening ClusterClient");
        let mut connection = client
            .get_async_connection()
            .await
            .expect("got redis connection");

        connection
        .del::<&'static str, ()>("plan\x005abb5fecf7df056396fb90fdf38d430b8c1fec55ec132fde878161608af18b76\x00{ topProducts { name name2:name } }\x00-")
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
            }))
            .unwrap()
            .schema(include_str!("fixtures/supergraph.graphql"))
            .build_supergraph()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .query(r#"{ topProducts { name name2:name } }"#)
            .method(Method::POST)
            .build()
            .unwrap();

        let res = supergraph
            .oneshot(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();

        println!("got res: {:?}", res);

        let s:String = connection
          .get("plan\x005abb5fecf7df056396fb90fdf38d430b8c1fec55ec132fde878161608af18b76\x00{ topProducts { name name2:name } }\x00-")
          .await
          .unwrap();
        println!("got from redis: {s}");
        let query_plan_res: serde_json::Value = serde_json::from_str(&s).unwrap();
        let query_plan: QueryPlan = serde_json::from_value(
            query_plan_res
                .as_object()
                .unwrap()
                .get("Ok")
                .unwrap()
                .get("Plan")
                .unwrap()
                .get("plan")
                .unwrap()
                .clone(),
        )
        .unwrap();
        insta::assert_debug_snapshot!(query_plan);
    }

    #[derive(Deserialize, Serialize)]

    struct QueryPlannerContent {
        plan: QueryPlan,
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apq() {
        let client = Client::open("redis://127.0.0.1:6379").expect("opening ClusterClient");
        let mut connection = client
            .get_async_connection()
            .await
            .expect("got redis connection");

        let config = json!({
            "supergraph": {
                "apq": {
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
        });

        let router = apollo_router::TestHarness::builder()
            .with_subgraph_network_requests()
            .configuration_json(config.clone())
            .unwrap()
            .schema(include_str!("fixtures/supergraph.graphql"))
            .build_router()
            .await
            .unwrap();

        let query_hash = "4c45433039407593557f8a982dafd316a66ec03f0e1ed5fa1b7ef8060d76e8ec";

        connection
            .del::<String, ()>(format!("apq\x00{query_hash}"))
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
        assert_eq!(res.errors.get(0).unwrap().message, "PersistedQueryNotFound");

        println!("got res: {:?}", res);

        let r: Option<String> = connection
            .get(&format!("apq\x00{query_hash}"))
            .await
            .unwrap();
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
        println!("got res: {:?}", res);

        let s: Option<String> = connection
            .get(&format!("apq\x00{query_hash}"))
            .await
            .unwrap();
        insta::assert_display_snapshot!(s.unwrap());

        // we start a new router with the same config
        // it should have the same connection to Redis, but the in memory cache has been reset
        let router = apollo_router::TestHarness::builder()
            .with_subgraph_network_requests()
            .configuration_json(config.clone())
            .unwrap()
            .schema(include_str!("fixtures/supergraph.graphql"))
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
        println!("got res: {:?}", res);
    }
}
