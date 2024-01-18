//TODO move telemetry logging functionality to this file
#[cfg(test)]
mod test {
    use std::any::TypeId;
    use std::sync::Arc;

    use tower::BoxError;
    use tower::ServiceBuilder;
    use tower_service::Service;
    use tracing_futures::WithSubscriber;

    use crate::assert_snapshot_subscriber;
    use crate::plugin::Plugin;
    use crate::plugin::PluginInit;
    use crate::plugins::telemetry::Telemetry;
    use crate::services::router;
    use crate::services::supergraph;
    use crate::services::supergraph::Response;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_log_requests() {
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
                        .header("custom-header", "foobar")
                        .query("query { foo }")
                        .build()
                        .expect("expecting valid request"),
                    |_r| {
                        supergraph::Response::fake_builder()
                            .header("custom-header", "foobar")
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
    // The difference with this and the `RouterTestHarness` is that this has much less of the router being wired up and is useful for testing a single plugin in isolation.
    // For now let's try and increase the coverage of the telemetry plugin using this and see how it goes.
    // In particular the `RouterTestHarness` isn't good for testing things with logging.
    struct PluginTestHarness<T: Plugin> {
        plugin: T,
    }
    #[buildstructor::buildstructor]
    impl<T: Plugin> PluginTestHarness<T> {
        #[builder]
        async fn new(yaml: Option<&'static str>, supergraph: Option<&'static str>) -> Self {
            let factory = crate::plugin::plugins()
                .find(|factory| factory.type_id == TypeId::of::<T>())
                .expect("plugin not registered");
            let name = &factory.name.replace("apollo.", "");
            let config = yaml
                .map(|yaml| serde_yaml::from_str::<serde_yaml::Value>(yaml).unwrap())
                .unwrap_or_default()
                .as_mapping_mut()
                .unwrap()
                .remove(&serde_yaml::Value::String(name.to_string()))
                .expect("no config for plugin");
            let supergraph_sdl = supergraph
                .map(|s| Arc::new(s.to_string()))
                .unwrap_or_default();
            let plugin = T::new(PluginInit {
                config: serde_yaml::from_value(config).expect("config was invalid"),
                supergraph_sdl,
                notify: Default::default(),
            })
            .await
            .expect("failed to initialize plugin");

            Self { plugin }
        }

        #[allow(dead_code)]
        async fn call_router(
            &self,
            request: router::Request,
            response_fn: fn(router::Request) -> router::Response,
        ) -> Result<crate::services::router::Response, BoxError> {
            let service: router::BoxService = router::BoxService::new(
                ServiceBuilder::new()
                    .service_fn(move |req: router::Request| async move { Ok((response_fn)(req)) }),
            );

            self.plugin.router_service(service).call(request).await
        }

        async fn call_supergraph(
            &self,
            request: supergraph::Request,
            response_fn: fn(supergraph::Request) -> supergraph::Response,
        ) -> Result<Response, BoxError> {
            let service: supergraph::BoxService =
                supergraph::BoxService::new(ServiceBuilder::new().service_fn(
                    move |req: supergraph::Request| async move { Ok((response_fn)(req)) },
                ));

            self.plugin.supergraph_service(service).call(request).await
        }
    }
}
