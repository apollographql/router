use apollo_router_core::{
    register_plugin, Plugin, RouterRequest, RouterResponse, SubgraphRequest, SubgraphResponse,
};
use http::StatusCode;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tower::{util::BoxService, BoxError, ServiceExt};

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

    async fn new(configuration: Self::Config) -> Result<Self, BoxError> {
        Ok(Self {
            status_codes: configuration.status_codes,
        })
    }

    fn subgraph_service(
        &mut self,
        _name: &str,
        service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        let all_status_codes = self.status_codes.clone();
        service
            .map_response(move |res| {
                let response_status_code = res.response.status().as_u16();
                // if a response contains a status code we're watching...
                if all_status_codes.contains(&response_status_code) {
                    // upsert allows us to:
                    // - check for the presence of a value for `status_codes` (first parameter)
                    // update the value if present (second parameter)
                    // insert a value if not (third parameter)
                    res.context
                        .upsert(
                            &"status_code".to_string(),
                            |status_code: u16| {
                                // return the status code with the highest priority
                                for &code in all_status_codes.iter() {
                                    if code == response_status_code || code == status_code {
                                        return code;
                                    }
                                }
                                status_code
                            },
                            || res.response.status().as_u16(),
                        )
                        .expect("couldn't insert status codes");
                }
                res
            })
            .boxed()
    }

    // At this point, all subgraph_services will have pushed their status codes if they match the `watch list`.
    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
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
// apollo_router_core provides a lot of utilities that will allow you to craft requests, responses,
// and test your plugins in isolation:
#[cfg(test)]
mod tests {
    use crate::propagate_status_code::{PropagateStatusCode, PropagateStatusCodeConfig};
    use apollo_router_core::{
        plugin::utils, Plugin, RouterRequest, RouterResponse, SubgraphRequest, SubgraphResponse,
    };
    use http::StatusCode;
    use serde_json::json;
    use tower::ServiceExt;

    // This test ensures the router will be able to
    // find our `propagate_status_code` plugin,
    // and deserialize an yml configuration with a list of status_codes into it
    // see `router.yaml` for more information
    #[tokio::test]
    async fn plugin_registered() {
        apollo_router_core::plugins()
            .get("example.propagate_status_code")
            .expect("Plugin not found")
            .create_instance(&json!({ "status_codes" : [500, 403, 401] }))
            .await
            .unwrap();
    }

    // Unit testing this plugin will be a tad more complicated than testing the other ones.
    // We will first ensure the SubgraphService pushes the right status codes.
    //
    // We will then make sure the RouterService is able to turn the relevant ordered status codes
    // into the relevant http response status.

    #[tokio::test]
    async fn subgraph_service_shouldnt_add_matching_status_code() {
        let mut mock_service = utils::test::MockSubgraphService::new();

        // Return StatusCode::FORBIDDEN, which shall be added to our status_codes
        mock_service.expect_call().times(1).returning(move |_| {
            Ok(SubgraphResponse::fake_builder()
                .status_code(StatusCode::FORBIDDEN)
                .build())
        });

        let mock_service = mock_service.build();

        // In this service_stack, PropagateStatusCode is `decorating` or `wrapping` our mock_service.
        let service_stack = PropagateStatusCode::new(PropagateStatusCodeConfig {
            status_codes: vec![500, 403, 401],
        })
        .await
        .expect("couldn't create plugin")
        .subgraph_service("accounts", mock_service.boxed());

        let subgraph_request = SubgraphRequest::fake_builder().build();

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
        let mut mock_service = utils::test::MockSubgraphService::new();

        // Return StatusCode::OK, which shall NOT be added to our status_codes
        mock_service.expect_call().times(1).returning(move |_| {
            Ok(SubgraphResponse::fake_builder()
                .status_code(StatusCode::OK)
                .build())
        });

        let mock_service = mock_service.build();

        // In this service_stack, PropagateStatusCode is `decorating` or `wrapping` our mock_service.
        let service_stack = PropagateStatusCode::new(PropagateStatusCodeConfig {
            status_codes: vec![500, 403, 401],
        })
        .await
        .expect("couldn't create plugin")
        .subgraph_service("accounts", mock_service.boxed());

        let subgraph_request = SubgraphRequest::fake_builder().build();

        let service_response = service_stack.oneshot(subgraph_request).await.unwrap();

        // Make sure the extensions doesn't contain any status
        let received_status_codes: Option<u16> = service_response
            .context
            .get("status_code")
            .expect("couldn't access context");

        assert!(received_status_codes.is_none());
    }

    // Now that our status codes mechanism has been tested,
    // we can unit test the RouterService part of our plugin

    #[tokio::test]
    async fn router_service_override_status_code() {
        let mut mock_service = utils::test::MockRouterService::new();

        mock_service
            .expect_call()
            .times(1)
            .returning(move |router_request: RouterRequest| {
                let context = router_request.context;
                // Insert several status codes which shall override the router response status
                context
                    .insert(&"status_code".to_string(), json!(500))
                    .expect("couldn't insert status_code");

                RouterResponse::fake_builder().context(context).build()
            });

        let mock_service = mock_service.build();

        // StatusCode::INTERNAL_SERVER_ERROR should have precedence here
        let service_stack = PropagateStatusCode::new(PropagateStatusCodeConfig {
            status_codes: vec![500, 403, 401],
        })
        .await
        .expect("couldn't create plugin")
        .router_service(mock_service.boxed());

        let router_request = RouterRequest::fake_builder()
            .build()
            .expect("expecting valid request");

        let service_response = service_stack.oneshot(router_request).await.unwrap();

        assert_eq!(
            StatusCode::INTERNAL_SERVER_ERROR,
            service_response.response.status()
        );
    }

    #[tokio::test]
    async fn router_service_do_not_override_status_code() {
        let mut mock_service = utils::test::MockRouterService::new();

        mock_service
            .expect_call()
            .times(1)
            .returning(move |router_request: RouterRequest| {
                let context = router_request.context;
                // Don't insert any StatusCode
                RouterResponse::fake_builder().context(context).build()
            });

        let mock_service = mock_service.build();

        // In this service_stack, PropagateStatusCode is `decorating` or `wrapping` our mock_service.
        let service_stack = PropagateStatusCode::new(PropagateStatusCodeConfig {
            status_codes: vec![500, 403, 401],
        })
        .await
        .expect("couldn't create plugin")
        .router_service(mock_service.boxed());

        let router_request = RouterRequest::fake_builder()
            .build()
            .expect("expecting valid request");

        let service_response = service_stack.oneshot(router_request).await.unwrap();

        assert_eq!(StatusCode::OK, service_response.response.status());
    }
}
