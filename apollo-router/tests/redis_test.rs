#[cfg(all(target_os = "linux", target_arch = "x86_64", test))]
mod test {
    use apollo_router::{graphql, services::supergraph};
    use http::Method;
    use redis::AsyncCommands;
    use redis_cluster_async::Client;
    use serde_json::json;
    use tower::ServiceExt;

    #[tokio::test(flavor = "multi_thread")]
    async fn query_planner() {
        let router = setup_router(json!({
            "supergraph": {
                "query_planning": {
                    "experimental_cache": {
                        "in_memory": {
                            "limit": 2
                        },
                        "redis": {
                            "urls": ["redis://:router@127.0.0.1:6379", "redis://:router@127.0.0.1:6380", "redis://:router@127.0.0.1:6381"]
                        }
                    }
                }
            }
        })).await;

        let request = supergraph::Request::fake_builder()
            .query(r#"{ topProducts { name name2:name } }"#)
            .method(Method::POST)
            .build()
            .unwrap();

        let res = query_with_router(router.clone(), request).await;

        println!("got res: {:?}", res);
        let client = Client::open(vec![
            "redis://:router@127.0.0.1:6379",
            "redis://:router@127.0.0.1:6380",
            "redis://:router@127.0.0.1:6381",
        ])
        .expect("opening ClusterClient");
        let mut connection = client.get_connection().await.expect("got redis connection");

        let s:String = connection
          .get("plan\05abb5fecf7df056396fb90fdf38d430b8c1fec55ec132fde878161608af18b76\0{ topProducts { name name2:name } }\0-")
          .await
          .unwrap();
        let query_plan: serde_json::Value = serde_json::from_str(&s).unwrap();
        insta::assert_json_snapshot!(query_plan);
    }

    async fn setup_router(config: serde_json::Value) -> supergraph::BoxCloneService {
        let router = apollo_router::TestHarness::builder()
            .with_subgraph_network_requests()
            .configuration_json(config)
            .unwrap()
            .schema(include_str!("fixtures/supergraph.graphql"))
            .build()
            .await
            .unwrap();
        router
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
