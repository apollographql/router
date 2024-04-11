//TODO move telemetry logging functionality to this file
#[cfg(test)]
pub(crate) mod test {
    use std::any::TypeId;

    use tower::BoxError;
    use tower::ServiceBuilder;
    use tower_service::Service;
    use tracing_futures::WithSubscriber;

    use crate::assert_snapshot_subscriber;
    use crate::graphql;
    use crate::plugin::DynPlugin;
    use crate::plugin::Plugin;
    use crate::plugins::telemetry::Telemetry;
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
            .yaml(include_str!(
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

    // Maybe factor this out after making it more usable
    // The difference with this and the `TestHarness` is that this has much less of the router being wired up and is useful for testing a single plugin in isolation.
    // In particular the `TestHarness` isn't good for testing things with logging.
    // For now let's try and increase the coverage of the telemetry plugin using this and see how it goes.

    pub(crate) struct PluginTestHarness<T: Plugin> {
        plugin: Box<dyn DynPlugin>,
        phantom: std::marker::PhantomData<T>,
    }
    #[buildstructor::buildstructor]
    impl<T: Plugin> PluginTestHarness<T> {
        #[builder(visibility = "pub(crate)")]
        async fn new(yaml: Option<&'static str>) -> Self {
            let factory = crate::plugin::plugins()
                .find(|factory| factory.type_id == TypeId::of::<T>())
                .expect("plugin not registered");
            let name = &factory.name.replace("apollo.", "");
            let config = yaml
                .map(|yaml| serde_yaml::from_str::<serde_json::Value>(yaml).unwrap())
                .map(|mut config| {
                    config
                        .as_object_mut()
                        .expect("invalid yaml")
                        .remove(name)
                        .expect("no config for plugin")
                })
                .unwrap_or_else(|| serde_json::Value::Object(Default::default()));

            let plugin = factory
                .create_instance_without_schema(&config)
                .await
                .expect("failed to create plugin");

            Self {
                plugin,
                phantom: Default::default(),
            }
        }

        #[allow(dead_code)]
        pub(crate) async fn call_router(
            &self,
            request: router::Request,
            response_fn: fn(router::Request) -> router::Response,
        ) -> Result<router::Response, BoxError> {
            let service: router::BoxService = router::BoxService::new(
                ServiceBuilder::new()
                    .service_fn(move |req: router::Request| async move { Ok((response_fn)(req)) }),
            );

            self.plugin.router_service(service).call(request).await
        }

        pub(crate) async fn call_supergraph(
            &self,
            request: supergraph::Request,
            response_fn: fn(supergraph::Request) -> supergraph::Response,
        ) -> Result<supergraph::Response, BoxError> {
            let service: supergraph::BoxService =
                supergraph::BoxService::new(ServiceBuilder::new().service_fn(
                    move |req: supergraph::Request| async move { Ok((response_fn)(req)) },
                ));

            self.plugin.supergraph_service(service).call(request).await
        }

        pub(crate) async fn call_subgraph(
            &self,
            request: subgraph::Request,
            response_fn: fn(subgraph::Request) -> subgraph::Response,
        ) -> Result<subgraph::Response, BoxError> {
            let name = request.subgraph_name.clone();
            let service: subgraph::BoxService =
                subgraph::BoxService::new(ServiceBuilder::new().service_fn(
                    move |req: subgraph::Request| async move { Ok((response_fn)(req)) },
                ));

            self.plugin
                .subgraph_service(&name.expect("subgraph name must be populated"), service)
                .call(request)
                .await
        }
    }
}
