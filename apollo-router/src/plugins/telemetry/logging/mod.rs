//TODO move telemetry logging functionality to this file
#[cfg(test)]
mod test {
    use tracing_futures::WithSubscriber;

    use crate::assert_snapshot_subscriber;
    use crate::graphql;
    use crate::plugins::telemetry::Telemetry;
    use crate::plugins::test::PluginTestHarness;
    use crate::services::router;
    use crate::services::subgraph;
    use crate::services::supergraph;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_router_service() {
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .build()
            .await
            .expect("test harness");

        async {
            let mut response = test_harness
                .router_service(|_r| async {
                    tracing::info!("response");
                    Ok(router::Response::fake_builder()
                        .header("custom-header", "val1")
                        .data(serde_json::json!({"data": "res"}))
                        .build()
                        .expect("expecting valid response"))
                })
                .call(
                    router::Request::fake_builder()
                        .body(router::body::from_bytes("query { foo }"))
                        .build()
                        .expect("expecting valid request"),
                )
                .await
                .expect("expecting successful response");

            response.next_response().await;
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_supergraph_service() {
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .build()
            .await
            .expect("test harness");

        async {
            let mut response = test_harness
                .supergraph_service(|_r| async {
                    tracing::info!("response");
                    supergraph::Response::fake_builder()
                        .header("custom-header", "val1")
                        .data(serde_json::json!({"data": "res"}))
                        .build()
                })
                .call(
                    supergraph::Request::fake_builder()
                        .query("query { foo }")
                        .variable("a", "b")
                        .build()
                        .expect("expecting valid request"),
                )
                .await
                .expect("expecting successful response");

            response.next_response().await;
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_service() {
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .build()
            .await
            .expect("test harness");

        async {
            test_harness
                .subgraph_service("subgraph", |_r| async {
                    tracing::info!("response");
                    subgraph::Response::fake2_builder()
                        .header("custom-header", "val1")
                        .data(serde_json::json!({"data": "res"}).to_string())
                        .subgraph_name("subgraph")
                        .build()
                })
                .call(
                    subgraph::Request::fake_builder()
                        .subgraph_name("subgraph")
                        .subgraph_request(http::Request::new(
                            graphql::Request::fake_builder()
                                .query("query { foo }")
                                .build(),
                        ))
                        .build(),
                )
                .await
                .expect("expecting successful response");
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await
    }
}
