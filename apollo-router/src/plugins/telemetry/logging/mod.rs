//TODO move telemetry logging functionality to this file
#[cfg(test)]
mod test {
    use tower::Service as _;
    use tower::ServiceBuilder;
    use tower::ServiceExt as _;
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
            let (mock_service, mut handle) =
                tower_test::mock::pair::<subgraph::Request, subgraph::Response>();
            let driver = tokio::spawn(async move {
                let (_req, responder) = handle.next_request().await.unwrap();
                responder.send_response(
                    subgraph::Response::fake2_builder()
                        .header("custom-header", "val1")
                        .data(serde_json::json!({"data": "res"}).to_string())
                        .subgraph_name("subgraph")
                        .build()
                        .unwrap(),
                );
            });

            let mut service = ServiceBuilder::new()
                .layer(test_harness.instrument_subgraph_layer())
                // the spawned driver doesn't actually run _inside_ the subgraph span. it's a
                // tower-test thing. map_response is one way to test what regular inner services do
                .map_response(|resp: subgraph::Response| {
                    tracing::info!("response");
                    resp
                })
                .service(mock_service);

            service
                .ready()
                .await
                .unwrap()
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

            crate::plugin::test::await_mock_driver(driver).await;
        }
        .with_subscriber(assert_snapshot_subscriber!())
        .await
    }
}
