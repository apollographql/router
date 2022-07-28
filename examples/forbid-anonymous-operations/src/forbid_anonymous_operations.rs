use std::ops::ControlFlow;

use apollo_router::graphql;
use apollo_router::layers::ServiceBuilderExt;
use apollo_router::plugin::Plugin;
use apollo_router::register_plugin;
use apollo_router::services::RouterRequest;
use apollo_router::services::RouterResponse;
use http::StatusCode;
use tower::util::BoxService;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

#[derive(Default)]
// Global state for our plugin would live here.
// We don't need any in this example
struct ForbidAnonymousOperations {}

#[async_trait::async_trait]
impl Plugin for ForbidAnonymousOperations {
    // We either forbid anonymous operations,
    // Or we don't. This is the reason why we don't need
    // to deserialize any configuration from a .yaml file.
    //
    // Config is a unit, and `ForbidAnonymousOperation` derives default.
    type Config = ();

    async fn new(_configuration: Self::Config) -> Result<Self, BoxError> {
        Ok(Self::default())
    }

    // Forbidding anonymous operations can happen at the very beginning of our GraphQL request lifecycle.
    // We will thus put the logic it in the `router_service` section of our plugin.
    fn router_service(
        &self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        // `ServiceBuilder` provides us with a `checkpoint` method.
        //
        // This method allows us to return ControlFlow::Continue(request) if we want to let the request through,
        // or ControlFlow::Return(response) with a crafted response if we don't want the request to go through.
        ServiceBuilder::new()
            .checkpoint(|req: RouterRequest| {
                // The http_request is stored in a `RouterRequest` context.
                // Its `body()` is an `apollo_router::Request`, that contains:
                // - Zero or one query
                // - Zero or one operation_name
                // - Zero or more variables
                // - Zero or more extensions
                let maybe_operation_name = req.originating_request.body().operation_name.as_ref();
                if maybe_operation_name.is_none()
                    || maybe_operation_name
                        .expect("is_none() has been checked before; qed")
                        .is_empty()
                {
                    // let's log the error
                    tracing::error!("Operation is not allowed!");

                    // Prepare an HTTP 400 response with a GraphQL error message
                    let res = RouterResponse::error_builder()
                        .error(graphql::Error {
                            message: "Anonymous operations are not allowed".to_string(),
                            ..Default::default()
                        })
                        .status_code(StatusCode::BAD_REQUEST)
                        .context(req.context)
                        .build()?;
                    Ok(ControlFlow::Break(res))
                } else {
                    // we're good to go!
                    tracing::info!("operation is allowed!");
                    Ok(ControlFlow::Continue(req))
                }
            })
            .service(service)
            .boxed()
    }
}

// This macro allows us to use it in our plugin registry!
// register_plugin takes a group name, and a plugin name.
//
// In order to keep the plugin names consistent,
// we use using the `Reverse domain name notation`
register_plugin!(
    "example",
    "forbid_anonymous_operations",
    ForbidAnonymousOperations
);

// Writing plugins means writing tests that make sure they behave as expected!
//
// apollo_router provides a lot of utilities that will allow you to craft requests, responses,
// and test your plugins in isolation:
#[cfg(test)]
mod tests {
    use apollo_router::graphql;
    use apollo_router::plugin::test;
    use apollo_router::plugin::Plugin;
    use apollo_router::services::RouterRequest;
    use apollo_router::services::RouterResponse;
    use http::StatusCode;
    use serde_json::Value;
    use tower::ServiceExt;

    use super::ForbidAnonymousOperations;

    // This test ensures the router will be able to
    // find our `forbid_anonymous_operations` plugin,
    // and deserialize an empty yml configuration into it
    // see router.yml for more information
    #[tokio::test]
    async fn plugin_registered() {
        apollo_router::plugin::plugins()
            .get("example.forbid_anonymous_operations")
            .expect("Plugin not found")
            .create_instance(&Value::Null)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_no_operation_name() {
        // create a mock service we will use to test our plugin
        // It does not have any behavior, because we do not expect it to be called.
        // If it is called, the test will panic,
        // letting us know ForbidAnonymousOperations did not behave as expected.
        let mock_service = test::MockRouterService::new().build();

        // In this service_stack, ForbidAnonymousOperations is `decorating` or `wrapping` our mock_service.
        let service_stack =
            ForbidAnonymousOperations::default().router_service(mock_service.boxed());

        // Let's create a request without an operation name...
        let request_without_any_operation_name = RouterRequest::fake_builder()
            .build()
            .expect("expecting valid request");

        // ...And call our service stack with it
        let mut service_response = service_stack
            .oneshot(request_without_any_operation_name)
            .await
            .unwrap();

        // ForbidAnonymousOperations should return a 400...
        assert_eq!(StatusCode::BAD_REQUEST, service_response.response.status());

        // with the expected error message
        let graphql_response: graphql::Response = service_response.next_response().await.unwrap();

        assert_eq!(
            "Anonymous operations are not allowed".to_string(),
            graphql_response.errors[0].message
        )
    }

    #[tokio::test]
    async fn test_empty_operation_name() {
        // create a mock service we will use to test our plugin
        // It does not have any behavior, because we do not expect it to be called.
        // If it is called, the test will panic,
        // letting us know ForbidAnonymousOperations did not behave as expected.
        let mock_service = test::MockRouterService::new().build();

        // In this service_stack, ForbidAnonymousOperations is `decorating` or `wrapping` our mock_service.
        let service_stack =
            ForbidAnonymousOperations::default().router_service(mock_service.boxed());

        // Let's create a request with an empty operation name...
        let request_with_empty_operation_name = RouterRequest::fake_builder()
            .operation_name("".to_string())
            .build()
            .expect("expecting valid request");

        // ...And call our service stack with it
        let mut service_response = service_stack
            .oneshot(request_with_empty_operation_name)
            .await
            .unwrap();

        // ForbidAnonymousOperations should return a 400...
        assert_eq!(StatusCode::BAD_REQUEST, service_response.response.status());

        // with the expected error message
        let graphql_response: graphql::Response = service_response.next_response().await.unwrap();

        assert_eq!(
            "Anonymous operations are not allowed".to_string(),
            graphql_response.errors[0].message
        )
    }

    #[tokio::test]
    async fn test_valid_operation_name() {
        let operation_name = "validOperationName";

        // create a mock service we will use to test our plugin
        let mut mock = test::MockRouterService::new();

        // The expected reply is going to be JSON returned in the RouterResponse { data } section.
        let expected_mock_response_data = "response created within the mock";

        // Let's set up our mock to make sure it will be called once, with the expected operation_name
        mock.expect_call()
            .times(1)
            .returning(move |req: RouterRequest| {
                assert_eq!(
                    operation_name,
                    // we're ok with unwrap's here because we're running a test
                    // we would not do this in actual code
                    req.originating_request
                        .body()
                        .operation_name
                        .as_ref()
                        .unwrap()
                );
                // let's return the expected data
                Ok(RouterResponse::fake_builder()
                    .data(expected_mock_response_data)
                    .build()
                    .unwrap())
            });

        // The mock has been set up, we can now build a service from it
        let mock_service = mock.build();

        // In this service_stack, ForbidAnonymousOperations is `decorating` or `wrapping` our mock_service.
        let service_stack =
            ForbidAnonymousOperations::default().router_service(mock_service.boxed());

        // Let's create a request with an valid operation name...
        let request_with_operation_name = RouterRequest::fake_builder()
            .operation_name(operation_name)
            .build()
            .expect("expecting valid request");

        // ...And call our service stack with it
        let mut service_response = service_stack
            .oneshot(request_with_operation_name)
            .await
            .unwrap();

        // Our stack should have returned an OK response...
        assert_eq!(StatusCode::OK, service_response.response.status());

        // ...with the expected data
        let graphql_response: graphql::Response = service_response.next_response().await.unwrap();

        assert_eq!(
            // we're allowed to unwrap() here because we know the json is a str()
            graphql_response.data.unwrap().as_str().unwrap(),
            expected_mock_response_data
        )
    }
}
