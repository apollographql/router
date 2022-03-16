use apollo_router_core::{
    register_plugin, Plugin, RouterRequest, RouterResponse, SubgraphRequest, SubgraphResponse,
};
use http::StatusCode;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tower::{util::BoxService, BoxError, ServiceExt};

#[derive(Serialize, Deserialize, JsonSchema)]
struct PropagateStatusCodeConfig {
    status_codes: Vec<u16>,
}

#[derive(Default)]
// Global state for our plugin would live here.
// We don't need any in this example
struct PropagateStatusCode {
    status_codes: Vec<u16>,
}

impl Plugin for PropagateStatusCode {
    // We either forbid anonymous operations,
    // Or we don't. This is the reason why we don't need
    // to deserialize any configuration from a .yml file.
    //
    // Config is a unit, and `ForbidAnonymousOperation` derives default.
    type Config = PropagateStatusCodeConfig;

    fn new(configuration: Self::Config) -> Result<Self, BoxError> {
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
                if all_status_codes.contains(&res.response.status().as_u16()) {
                    res.context
                        .upsert(
                            &"status_codes".to_string(),
                            |mut status_codes: Vec<u16>| {
                                status_codes.push(res.response.status().as_u16());
                                status_codes
                            },
                            || vec![res.response.status().as_u16()],
                        )
                        .expect("couldn't insert status codes");
                }
                res
            })
            .boxed()
    }

    // Forbidding anonymous operations can happen at the very beginning of our GraphQL request lifecycle.
    // We will thus put the logic it in the `router_service` section of our plugin.
    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        let all_status_codes = self.status_codes.clone();

        service
            .map_response(move |mut res| {
                if let Some(received_status_codes) = res
                    .context
                    .get::<&String, Vec<u16>>(&"status_codes".to_string())
                    .expect("couldn't access context")
                {
                    for code in all_status_codes {
                        if received_status_codes.contains(&code) {
                            *res.response.status_mut() =
                                StatusCode::from_u16(code).expect("status code should be valid");
                            break;
                        }
                    }
                }
                res
            })
            .boxed()
    }
}

// This macro allows us to use it in our plugin registry!
// register_plugin takes a group name, and a plugin name.
//
// In order to keep the plugin names consistent,
// we use using the `Reverse domain name notation`
register_plugin!("example", "propagate_status_code", PropagateStatusCode);

// Writing plugins means writing tests that make sure they behave as expected!
//
// apollo_router_core provides a lot of utilities that will allow you to craft requests, responses,
// and test your plugins in isolation:
#[cfg(test)]
mod tests {
    use crate::propagate_status_code::{PropagateStatusCode, PropagateStatusCodeConfig};
    use apollo_router_core::{plugin_utils, Plugin, RouterRequest};
    use http::StatusCode;
    use serde_json::json;
    use tower::ServiceExt;

    // This test ensures the router will be able to
    // find our `forbid_anonymous_operations` plugin,
    // and deserialize an empty yml configuration into it
    // see config.yml for more information
    #[tokio::test]
    async fn plugin_registered() {
        apollo_router_core::plugins()
            .get("example.propagate_status_code")
            .expect("Plugin not found")
            .create_instance(&json!({ "status_codes" : [500, 403, 401] }))
            .unwrap();
    }

    // Unit testing this plugin will be a tad more complicated than testing the other ones.
    // We will first ensure the SubgraphService pushes the right status codes.
    //
    // We will then make sure the RouterService is able to turn the relevant ordered status codes
    // into the relevant http response status.

    #[tokio::test]
    async fn subgraph_service_shouldnt_add_matching_status_code() {
        let mut mock_service = plugin_utils::MockSubgraphService::new();

        // Return StatusCode::FORBIDDEN, which shall be added to our status_codes
        mock_service.expect_call().times(1).returning(move |_| {
            Ok(plugin_utils::SubgraphResponse::builder()
                .status(StatusCode::FORBIDDEN)
                .build()
                .into())
        });

        let mock_service = mock_service.build();

        // In this service_stack, PropagateStatusCode is `decorating` or `wrapping` our mock_service.
        let service_stack = PropagateStatusCode::new(PropagateStatusCodeConfig {
            status_codes: vec![500, 403, 401],
        })
        .expect("couldn't create plugin")
        .subgraph_service("accounts", mock_service.boxed());

        let subgraph_request = plugin_utils::SubgraphRequest::builder().build().into();

        let service_response = service_stack.oneshot(subgraph_request).await.unwrap();

        // Make sure the extensions doesn't contain any status
        let received_status_codes: Vec<u16> = service_response
            .context
            .get("status_codes")
            .expect("couldn't access context")
            .expect("couldn't access status_codes");

        assert!(received_status_codes.contains(&403));
    }

    #[tokio::test]
    async fn subgraph_service_shouldnt_add_not_matching_status_code() {
        let mut mock_service = plugin_utils::MockSubgraphService::new();

        // Return StatusCode::OK, which shall NOT be added to our status_codes
        mock_service.expect_call().times(1).returning(move |_| {
            Ok(plugin_utils::SubgraphResponse::builder()
                .status(StatusCode::OK)
                .build()
                .into())
        });

        let mock_service = mock_service.build();

        // In this service_stack, PropagateStatusCode is `decorating` or `wrapping` our mock_service.
        let service_stack = PropagateStatusCode::new(PropagateStatusCodeConfig {
            status_codes: vec![500, 403, 401],
        })
        .expect("couldn't create plugin")
        .subgraph_service("accounts", mock_service.boxed());

        let subgraph_request = plugin_utils::SubgraphRequest::builder().build().into();

        let service_response = service_stack.oneshot(subgraph_request).await.unwrap();

        // Make sure the extensions doesn't contain any status
        let received_status_codes: Option<Vec<u16>> = service_response
            .context
            .get("status_codes")
            .expect("couldn't access context");

        assert!(received_status_codes.is_none());
    }

    // Now that our status codes mechanism has been tested,
    // we can unit test the RouterService part of our plugin

    #[tokio::test]
    async fn router_service_override_status_code() {
        let mut mock_service = plugin_utils::MockRouterService::new();

        mock_service
            .expect_call()
            .times(1)
            .returning(move |router_request: RouterRequest| {
                let context = router_request.context;
                // Insert StatusCode::FORBIDDEN, which shall override the router response status
                context
                    .insert(&"status_codes".to_string(), json!([403u16]))
                    .expect("couldn't insert status_code");

                Ok(plugin_utils::RouterResponse::builder()
                    .context(context.into())
                    .build()
                    .into())
            });

        let mock_service = mock_service.build();

        // In this service_stack, PropagateStatusCode is `decorating` or `wrapping` our mock_service.
        let service_stack = PropagateStatusCode::new(PropagateStatusCodeConfig {
            status_codes: vec![500, 403, 401],
        })
        .expect("couldn't create plugin")
        .router_service(mock_service.boxed());

        let router_request = plugin_utils::RouterRequest::builder().build().into();

        let service_response = service_stack.oneshot(router_request).await.unwrap();

        assert_eq!(StatusCode::FORBIDDEN, service_response.response.status());
    }

    #[tokio::test]
    async fn router_service_do_not_override_status_code() {
        let mut mock_service = plugin_utils::MockRouterService::new();

        mock_service
            .expect_call()
            .times(1)
            .returning(move |router_request: RouterRequest| {
                let context = router_request.context;
                // Insert a StatusCode that isn't part of our PropagateStatusCode configuration
                // which shall NOT override the response status
                context
                    .insert(&"status_codes".to_string(), json!([418u16]))
                    .expect("couldn't insert status_code");

                Ok(plugin_utils::RouterResponse::builder()
                    .context(context.into())
                    .build()
                    .into())
            });

        let mock_service = mock_service.build();

        // In this service_stack, PropagateStatusCode is `decorating` or `wrapping` our mock_service.
        let service_stack = PropagateStatusCode::new(PropagateStatusCodeConfig {
            status_codes: vec![500, 403, 401],
        })
        .expect("couldn't create plugin")
        .router_service(mock_service.boxed());

        let router_request = plugin_utils::RouterRequest::builder().build().into();

        let service_response = service_stack.oneshot(router_request).await.unwrap();

        assert_eq!(StatusCode::OK, service_response.response.status());
    }
}
