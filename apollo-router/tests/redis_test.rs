#[cfg(all(target_os = "linux", target_arch = "x86_64", test))]
mod test {
    use apollo_router::graphql;
    use apollo_router::services::execution::QueryPlan;
    use apollo_router::services::supergraph;
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

        let router = setup_router(json!({
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
        .await;

        let request = supergraph::Request::fake_builder()
            .query(r#"{ topProducts { name name2:name } }"#)
            .method(Method::POST)
            .build()
            .unwrap();

        let res = query_with_router(router.clone(), request).await;

        println!("got res: {:?}", res);

        let s:String = connection
          .get("plan\x005abb5fecf7df056396fb90fdf38d430b8c1fec55ec132fde878161608af18b76\x00{ topProducts { name name2:name } }\x00-")
          .await
          .unwrap();
        let query_plan: QueryPlannerContent = serde_json::from_str(&s).unwrap();
        insta::assert_json_snapshot!(serde_json::to_value(query_plan).unwrap());
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

        let router = setup_router(config.clone()).await;

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
        let request = supergraph::Request::fake_builder()
            .extension("persistedQuery", persisted.clone())
            .method(Method::POST)
            .build()
            .unwrap();

        let res = query_with_router(router.clone(), request).await;
        assert_eq!(res.errors.get(0).unwrap().message, "PersistedQueryNotFound");

        println!("got res: {:?}", res);

        let r: Option<String> = connection
            .get(&format!("apq\x00{query_hash}"))
            .await
            .unwrap();
        assert!(r.is_none());

        // Now we register the query
        // it should set a value in Redis
        let request = supergraph::Request::fake_builder()
            .query(r#"{ topProducts { name name2:name } }"#)
            .extension("persistedQuery", persisted.clone())
            .method(Method::POST)
            .build()
            .unwrap();

        let res = query_with_router(router.clone(), request).await;
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
        let router = setup_router(config.clone()).await;

        // a request with only the hash should succeed because it is stored in Redis
        let request = supergraph::Request::fake_builder()
            .extension("persistedQuery", persisted.clone())
            .method(Method::POST)
            .build()
            .unwrap();

        let res = query_with_router(router.clone(), request).await;
        assert!(res.data.is_some());
        assert!(res.errors.is_empty());
        println!("got res: {:?}", res);
    }

    async fn setup_router(config: serde_json::Value) -> supergraph::BoxCloneService {
        apollo_router::TestHarness::builder()
            .with_subgraph_network_requests()
            .configuration_json(config)
            .unwrap()
            .schema(include_str!("fixtures/supergraph.graphql"))
            .build_supergraph()
            .await
            .unwrap()
    }

    async fn query_with_router(
        router: supergraph::BoxCloneService,
        request: supergraph::Request,
    ) -> graphql::Response {
        router
            .oneshot(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap()
    }
}
