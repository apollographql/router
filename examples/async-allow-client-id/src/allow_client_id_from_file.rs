use apollo_router_core::{
    plugin_utils, register_plugin, Plugin, RouterRequest, RouterResponse, ServiceBuilderExt,
};
use http::StatusCode;
use schemars::JsonSchema;
use serde::Deserialize;
use std::{ops::ControlFlow, path::PathBuf};
use tower::{util::BoxService, BoxError, ServiceBuilder, ServiceExt};

// This structure is the one we'll deserialize the yml configuration into
#[derive(Deserialize, JsonSchema)]
struct AllowClientIdConfig {
    header: String,
    path: String,
}

struct AllowClientIdFromFile {
    header: String,
    allowed_ids_path: PathBuf,
}

impl Plugin for AllowClientIdFromFile {
    type Config = AllowClientIdConfig;

    fn new(configuration: Self::Config) -> Result<Self, BoxError> {
        let AllowClientIdConfig { path, header } = configuration;
        let allowed_ids_path = PathBuf::from(path.as_str());
        Ok(Self {
            allowed_ids_path,
            header,
        })
    }

    // On each request, this plugin will extract a x-client-id header, and check against a file
    // whether the client is allowed to run a request.
    //
    // While this is not the most performant and efficient usecase,
    // We could easily change the place where the file list is stored,
    // switching the async file read with an async http request
    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        let header_key = self.header.clone();
        // async_checkpoint is an async function.
        // this means it will run whenever the service `await`s it
        // given we're getting a mutable reference to self,
        // self won't be present anymore when we `await` the checkpoint.
        //
        // this is solved by cloning the path and moving it into the async_checkpoint callback.
        //
        // see https://rust-lang.github.io/async-book/03_async_await/01_chapter.html#async-lifetimes for more information
        let allowed_ids_path = self.allowed_ids_path.clone();

        // `ServiceBuilder` provides us with an `async_checkpoint` method.
        //
        // This method allows us to return ControlFlow::Continue(request) if we want to let the request through,
        // or ControlFlow::Return(response) with a crafted response if we don't want the request to go through.
        ServiceBuilder::new()
            .async_checkpoint(move |req: RouterRequest| {
                // The http_request is stored in a `RouterRequest` context.
                // We are going to check the headers for the presence of the header we're looking for
                if !req.context.request.headers().contains_key(&header_key) {
                    // Prepare an HTTP 401 response with a GraphQL error message
                    let res = plugin_utils::RouterResponse::builder()
                        .errors(vec![apollo_router_core::Error {
                            message: format!("Missing '{header_key}' header"),
                            ..Default::default()
                        }])
                        .build()
                        .with_status(StatusCode::UNAUTHORIZED);
                    return Box::pin(async { Ok(ControlFlow::Break(res)) });
                }

                // It is best practice to perform checks before we unwrap,
                // And to use `expect()` instead of `unwrap()`, with a message
                // that explains why the use of `expect()` is safe
                let client_id = req
                    .context
                    .request
                    .headers()
                    .get("x-client-id")
                    .expect("this cannot fail; we checked for header presence above;qed")
                    .to_str();

                let client_id_string = match client_id {
                    Ok(client_id) => client_id.to_string(),
                    Err(_not_a_string_error) => {
                        // Prepare an HTTP 400 response with a GraphQL error message
                        let res = plugin_utils::RouterResponse::builder()
                            .errors(vec![apollo_router_core::Error {
                                message: format!("'{header_key}' value is not a string"),
                                ..Default::default()
                            }])
                            .build()
                            .with_status(StatusCode::BAD_REQUEST);
                        return Box::pin(async { Ok(ControlFlow::Break(res)) });
                    }
                };

                // like at the beginning of this function call, we are about to return a future,
                // which will run whenever the service `await`s it.
                // we need allowed_ids_path to be available for the spawned future,
                // but also for any future (pun intended) call / request that will be made against this service
                //
                // This is why we will give our future a clone of the allowed ids.
                //
                // see https://rust-lang.github.io/async-book/03_async_await/01_chapter.html#async-lifetimes for more information
                let allowed_ids_path = allowed_ids_path.clone();
                Box::pin(async move {
                    let allowed_clients: Vec<String> = serde_json::from_str(
                        tokio::fs::read_to_string(allowed_ids_path)
                            .await
                            .unwrap()
                            .as_str(),
                    )
                    .unwrap();

                    if allowed_clients.contains(&client_id_string) {
                        Ok(ControlFlow::Continue(req))
                    } else {
                        // Prepare an HTTP 403 response with a GraphQL error message
                        let res = plugin_utils::RouterResponse::builder()
                            .errors(vec![apollo_router_core::Error {
                                message: "client-id is not allowed".to_string(),
                                ..Default::default()
                            }])
                            .build()
                            .with_status(StatusCode::FORBIDDEN);
                        Ok(ControlFlow::Break(res))
                    }
                })
            })
            // Given the async nature of our checkpoint, we need to make sure
            // the underlying service will be available whenever the checkpoint
            // returns ControlFlow::Continue.
            // This is achieved by adding a buffer in front of the service,
            // and (automatically) giving one `slot` to our async_checkpoint
            //
            // forgetting to add .buffer() here will trigger a compilation error.
            .buffer(20_000)
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
    "allow-client-id-from-file",
    AllowClientIdFromFile
);

// Writing plugins means writing tests that make sure they behave as expected!
//
// apollo_router_core provides a lot of utilities that will allow you to craft requests, responses,
// and test your plugins in isolation:
#[cfg(test)]
mod tests {
    use crate::allow_client_id_from_file::AllowClientIdConfig;

    use super::AllowClientIdFromFile;
    use apollo_router_core::{plugin_utils, Plugin, RouterRequest};
    use http::StatusCode;
    use serde_json::json;
    use tower::ServiceExt;

    // This test ensures the router will be able to
    // find our `allow-client-id-from-file` plugin,
    // and deserialize an empty yml configuration containing a path
    // see config.yml for more information
    #[tokio::test]
    async fn plugin_registered() {
        apollo_router_core::plugins()
            .get("example.allow-client-id-from-file")
            .expect("Plugin not found")
            .create_instance(&json!({"header": "x-client-id","path": "allowedClientIds.json"}))
            .unwrap();
    }

    #[tokio::test]
    async fn test_no_client_id() {
        // create a mock service we will use to test our plugin
        // It does not have any behavior, because we do not expect it to be called.
        // If it is called, the test will panic,
        // letting us know AllowClientIdFromFile did not behave as expected.
        let mock_service = plugin_utils::MockRouterService::new().build();

        // In this service_stack, AllowClientIdFromFile is `decorating` or `wrapping` our mock_service.
        let service_stack = AllowClientIdFromFile::new(AllowClientIdConfig {
            path: "allowedClientIds.json".to_string(),
            header: "x-client-id".to_string(),
        })
        .expect("couldn't create AllowClientIdFromFile")
        .router_service(mock_service.boxed());

        // Let's create a request without a client id...
        let request_without_client_id = plugin_utils::RouterRequest::builder().build().into();

        // ...And call our service stack with it
        let service_response = service_stack
            .oneshot(request_without_client_id)
            .await
            .unwrap();

        // AllowClientIdFromFile should return a 401...
        assert_eq!(StatusCode::UNAUTHORIZED, service_response.response.status());

        // with the expected error message
        let graphql_response: apollo_router_core::Response =
            service_response.response.into_body().try_into().unwrap();

        assert_eq!(
            "Missing 'x-client-id' header".to_string(),
            graphql_response.errors[0].message
        )
    }

    #[tokio::test]
    async fn test_client_id_not_allowed() {
        // create a mock service we will use to test our plugin
        // It does not have any behavior, because we do not expect it to be called.
        // If it is called, the test will panic,
        // letting us know AllowClientIdFromFile did not behave as expected.
        let mock_service = plugin_utils::MockRouterService::new().build();

        // In this service_stack, AllowClientIdFromFile is `decorating` or `wrapping` our mock_service.
        let service_stack = AllowClientIdFromFile::new(AllowClientIdConfig {
            path: "allowedClientIds.json".to_string(),
            header: "x-client-id".to_string(),
        })
        .expect("couldn't create AllowClientIdFromFile")
        .router_service(mock_service.boxed());

        // Let's create a request with a not allowed client id...
        let request_with_unauthorized_client_id = plugin_utils::RouterRequest::builder()
            .headers(vec![(
                "x-client-id".to_string(),
                "invalid_client_id".to_string(),
            )])
            .build()
            .into();

        // ...And call our service stack with it
        let service_response = service_stack
            .oneshot(request_with_unauthorized_client_id)
            .await
            .unwrap();

        // AllowClientIdFromFile should return a 403...
        assert_eq!(StatusCode::FORBIDDEN, service_response.response.status());

        // with the expected error message
        let graphql_response: apollo_router_core::Response =
            service_response.response.into_body().try_into().unwrap();

        assert_eq!(
            "client-id is not allowed".to_string(),
            graphql_response.errors[0].message
        )
    }

    #[tokio::test]
    async fn test_client_id_allowed() {
        let valid_client_id = "jeremy";

        // create a mock service we will use to test our plugin
        let mut mock = plugin_utils::MockRouterService::new();

        // The expected reply is going to be JSON returned in the RouterResponse { data } section.
        let expected_mock_response_data = "response created within the mock";

        // Let's set up our mock to make sure it will be called once, with the expected operation_name
        mock.expect_call()
            .times(1)
            .returning(move |req: RouterRequest| {
                assert_eq!(
                    valid_client_id,
                    // we're ok with unwrap's here because we're running a test
                    // we would not do this in actual code
                    req.context
                        .request
                        .headers()
                        .get("x-client-id")
                        .unwrap()
                        .to_str()
                        .unwrap()
                );
                // let's return the expected data
                Ok(plugin_utils::RouterResponse::builder()
                    .data(expected_mock_response_data.into())
                    .build()
                    .into())
            });

        // The mock has been set up, we can now build a service from it
        let mock_service = mock.build();

        // In this service_stack, AllowClientIdFromFile is `decorating` or `wrapping` our mock_service.
        let service_stack = AllowClientIdFromFile::new(AllowClientIdConfig {
            path: "allowedClientIds.json".to_string(),
            header: "x-client-id".to_string(),
        })
        .expect("couldn't create AllowClientIdFromFile")
        .router_service(mock_service.boxed());

        // Let's create a request with an valid client id...
        let request_with_valid_client_id = plugin_utils::RouterRequest::builder()
            .headers(vec![(
                "x-client-id".to_string(),
                valid_client_id.to_string(),
            )])
            .build()
            .into();

        // ...And call our service stack with it
        let service_response = service_stack
            .oneshot(request_with_valid_client_id)
            .await
            .unwrap();

        // Our stack should have returned an OK response...
        assert_eq!(StatusCode::OK, service_response.response.status());

        // ...with the expected data
        let graphql_response: apollo_router_core::Response =
            service_response.response.into_body().try_into().unwrap();

        assert_eq!(
            // we're allowed to unwrap() here because we know the json is a str()
            graphql_response.data.as_str().unwrap(),
            expected_mock_response_data
        )
    }
}
