use apollo_router::plugin::Plugin;
use apollo_router::plugin::PluginInit;
use apollo_router::register_plugin;
use apollo_router::services::subgraph;
use apollo_router::services::supergraph;
use http::StatusCode;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;
use tower::ServiceExt;

// This configuration will be used
// to Deserialize the yml configuration
#[derive(Serialize, Deserialize, JsonSchema)]
struct PropagateStatusCodeConfig {
    status_codes: Vec<u16>,
}

#[derive(Default)]
struct PropagateStatusCode {
    // An ordered list of status codes to check
    status_codes: Vec<u16>,
}

#[async_trait::async_trait]
impl Plugin for PropagateStatusCode {
    type Config = PropagateStatusCodeConfig;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(Self {
            status_codes: init.config.status_codes,
        })
    }

    fn subgraph_service(&self, _name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
        let all_status_codes = self.status_codes.clone();
        service
            .map_response(move |res| {
                let response_status_code = res.response.status().as_u16();
                // if a response contains a status code we're watching...
                if all_status_codes.contains(&response_status_code) {
                    // upsert allows us to:
                    // - check for the presence of a value for `status_codes` (first parameter)
                    // update the value if present (second parameter)
                    res.context
                        .upsert(&"status_code".to_string(), |status_code: u16| {
                            // return the status code with the highest priority
                            for &code in all_status_codes.iter() {
                                if code == response_status_code || code == status_code {
                                    return code;
                                }
                            }
                            status_code
                        })
                        .expect("couldn't insert status codes");
                }
                res
            })
            .boxed()
    }

    // At this point, all subgraph_services will have pushed their status codes if they match the `watch list`.
    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        service
            .map_response(move |mut res| {
                if let Some(code) = res
                    .context
                    .get::<&String, u16>(&"status_code".to_string())
                    .expect("couldn't access context")
                {
                    *res.response.status_mut() =
                        StatusCode::from_u16(code).expect("status code should be valid");
                }
                res
            })
            .boxed()
    }
}

// This macro allows us to use it in our plugin registry!
// register_plugin takes a group name, and a plugin name.
register_plugin!("example", "propagate_status_code", PropagateStatusCode);

// Writing plugins means writing tests that make sure they behave as expected!
//
// apollo_router provides a lot of utilities that will allow you to craft requests, responses,
// and test your plugins in isolation:
#[cfg(test)]
mod tests {
    use apollo_router::plugin::test;
    use apollo_router::plugin::Plugin;
    use apollo_router::plugin::PluginInit;
    use apollo_router::services::subgraph;
    use apollo_router::services::supergraph;
    use http::StatusCode;
    use serde_json::json;
    use tower::ServiceExt;

    use crate::propagate_status_code::PropagateStatusCode;
    use crate::propagate_status_code::PropagateStatusCodeConfig;

    // This test ensures the router will be able to
    // find our `propagate_status_code` plugin,
    // and deserialize an yml configuration with a list of status_codes into it
    // see `router.yaml` for more information
    #[tokio::test]
    async fn plugin_registered() {
        let config = serde_json::json!({
            "plugins": {
                "example.propagate_status_code": {
                    "status_codes" : [500, 403, 401]
                }
            }
        });
        apollo_router::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .unwrap();
    }

    // Unit testing this plugin will be a tad more complicated than testing the other ones.
    // We will first ensure the SubgraphService pushes the right status codes.
    //
    // We will then make sure the SupergraphService is able to turn the relevant ordered status codes
    // into the relevant http response status.

    #[tokio::test]
    async fn subgraph_service_shouldnt_add_matching_status_code() {
        let mut mock_service = test::MockSubgraphService::new();

        // Return StatusCode::FORBIDDEN, which shall be added to our status_codes
        mock_service.expect_call().times(1).returning(move |_| {
            Ok(subgraph::Response::fake_builder()
                .status_code(StatusCode::FORBIDDEN)
                .build())
        });

        // In this service_stack, PropagateStatusCode is `decorating` or `wrapping` our mock_service.
        let init = PluginInit::fake_builder()
            .config(PropagateStatusCodeConfig {
                status_codes: vec![500, 403, 401],
            })
            .build();
        let service_stack = PropagateStatusCode::new(init)
            .await
            .expect("couldn't create plugin")
            .subgraph_service("accounts", mock_service.boxed());

        let subgraph_request = subgraph::Request::fake_builder().build();

        let service_response = service_stack.oneshot(subgraph_request).await.unwrap();

        // Make sure the extensions doesn't contain any status
        let received_status_code: u16 = service_response
            .context
            .get("status_code")
            .expect("couldn't access context")
            .expect("couldn't access status_codes");

        assert_eq!(403, received_status_code);
    }

    #[tokio::test]
    async fn subgraph_service_shouldnt_add_not_matching_status_code() {
        let mut mock_service = test::MockSubgraphService::new();

        // Return StatusCode::OK, which shall NOT be added to our status_codes
        mock_service.expect_call().times(1).returning(move |_| {
            Ok(subgraph::Response::fake_builder()
                .status_code(StatusCode::OK)
                .build())
        });

        // In this service_stack, PropagateStatusCode is `decorating` or `wrapping` our mock_service.
        let init = PluginInit::fake_builder()
            .config(PropagateStatusCodeConfig {
                status_codes: vec![500, 403, 401],
            })
            .build();
        let service_stack = PropagateStatusCode::new(init)
            .await
            .expect("couldn't create plugin")
            .subgraph_service("accounts", mock_service.boxed());

        let subgraph_request = subgraph::Request::fake_builder().build();

        let service_response = service_stack.oneshot(subgraph_request).await.unwrap();

        // Make sure the extensions doesn't contain any status
        let received_status_codes: Option<u16> = service_response
            .context
            .get("status_code")
            .expect("couldn't access context");

        assert!(received_status_codes.is_none());
    }

    // Now that our status codes mechanism has been tested,
    // we can unit test the SupergraphService part of our plugin

    #[tokio::test]
    async fn router_service_override_status_code() {
        let mut mock_service = test::MockSupergraphService::new();

        mock_service.expect_call().times(1).returning(
            move |router_request: supergraph::Request| {
                let context = router_request.context;
                // Insert several status codes which shall override the router response status
                context
                    .insert(&"status_code".to_string(), json!(500u16))
                    .expect("couldn't insert status_code");

                Ok(supergraph::Response::fake_builder()
                    .context(context)
                    .build()
                    .unwrap())
            },
        );

        // StatusCode::INTERNAL_SERVER_ERROR should have precedence here
        let init = PluginInit::fake_builder()
            .config(PropagateStatusCodeConfig {
                status_codes: vec![500, 403, 401],
            })
            .build();
        let service_stack = PropagateStatusCode::new(init)
            .await
            .expect("couldn't create plugin")
            .supergraph_service(mock_service.boxed());

        let router_request = supergraph::Request::fake_builder()
            .build()
            .expect("expecting valid request");

        let mut service_response = service_stack.oneshot(router_request).await.unwrap();

        assert_eq!(
            StatusCode::INTERNAL_SERVER_ERROR,
            service_response.response.status()
        );

        let _response = service_response.next_response().await.unwrap();
    }

    #[tokio::test]
    async fn router_service_do_not_override_status_code() {
        let mut mock_service = test::MockSupergraphService::new();

        mock_service.expect_call().times(1).returning(
            move |router_request: supergraph::Request| {
                let context = router_request.context;
                // Don't insert any StatusCode
                Ok(supergraph::Response::fake_builder()
                    .context(context)
                    .build()
                    .unwrap())
            },
        );

        // In this service_stack, PropagateStatusCode is `decorating` or `wrapping` our mock_service.
        let init = PluginInit::fake_builder()
            .config(PropagateStatusCodeConfig {
                status_codes: vec![500, 403, 401],
            })
            .build();
        let service_stack = PropagateStatusCode::new(init)
            .await
            .expect("couldn't create plugin")
            .supergraph_service(mock_service.boxed());

        let router_request = supergraph::Request::fake_builder()
            .build()
            .expect("expecting valid request");

        let mut service_response = service_stack.oneshot(router_request).await.unwrap();

        assert_eq!(StatusCode::OK, service_response.response.status());
        let _response = service_response.next_response().await.unwrap();
    }
}
