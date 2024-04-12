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
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder().build().await;

        async {
            let mut response = test_harness
                .call_router(
                    router::Request::fake_builder()
                        .body("query { foo }")
                        .build()
                        .expect("expecting valid request"),
                    |_r| {
                        tracing::info!("response");
                        router::Response::fake_builder()
                            .header("custom-header", "val1")
                            .data(serde_json::json!({"data": "res"}))
                            .build()
                            .expect("expecting valid response")
                    },
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
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder().build().await;

        async {
            let mut response = test_harness
                .call_supergraph(
                    supergraph::Request::fake_builder()
                        .query("query { foo }")
                        .variable("a", "b")
                        .build()
                        .expect("expecting valid request"),
                    |_r| {
                        tracing::info!("response");
                        supergraph::Response::fake_builder()
                            .header("custom-header", "val1")
                            .data(serde_json::json!({"data": "res"}))
                            .build()
                            .expect("expecting valid response")
                    },
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
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder().build().await;

        async {
            test_harness
                .call_subgraph(
                    subgraph::Request::fake_builder()
                        .subgraph_name("subgraph")
                        .subgraph_request(http::Request::new(
                            graphql::Request::fake_builder()
                                .query("query { foo }")
                                .build(),
                        ))
                        .build(),
                    |_r| {
                        tracing::info!("response");
                        subgraph::Response::fake2_builder()
                            .header("custom-header", "val1")
                            .data(serde_json::json!({"data": "res"}).to_string())
                            .build()
                            .expect("expecting valid response")
                    },
                )
                .await
                .expect("expecting successful response");
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_when_header() {
        let test_harness: PluginTestHarness<Telemetry> = PluginTestHarness::builder()
            .config(include_str!(
                "testdata/experimental_when_header.router.yaml"
            ))
            .build()
            .await;

        async {
            let mut response = test_harness
                .call_supergraph(
                    supergraph::Request::fake_builder()
                        .header("custom-header1", "val1")
                        .header("custom-header2", "val2")
                        .query("query { foo }")
                        .build()
                        .expect("expecting valid request"),
                    |_r| {
                        tracing::info!("response");
                        supergraph::Response::fake_builder()
                            .header("custom-header1", "val1")
                            .header("custom-header2", "val2")
                            .data(serde_json::json!({"data": "res"}))
                            .build()
                            .expect("expecting valid response")
                    },
                )
                .await
                .expect("expecting successful response");

            response.next_response().await;
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await
    }
}
