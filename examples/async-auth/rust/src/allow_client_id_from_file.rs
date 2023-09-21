use std::ops::ControlFlow;
use std::path::PathBuf;

use apollo_router::graphql;
use apollo_router::layers::ServiceBuilderExt;
use apollo_router::plugin::Plugin;
use apollo_router::plugin::PluginInit;
use apollo_router::register_plugin;
use apollo_router::services::supergraph;
use http::StatusCode;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json_bytes::Value;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

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

#[async_trait::async_trait]
impl Plugin for AllowClientIdFromFile {
    type Config = AllowClientIdConfig;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let AllowClientIdConfig { path, header } = init.config;
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
    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        let header_key = self.header.clone();
        // oneshot_async_checkpoint is an async function.
        // this means it will run whenever the service `await`s it
        // given we're getting a mutable reference to self,
        // self won't be present anymore when we `await` the checkpoint.
        //
        // this is solved by cloning the path and moving it into the oneshot_async_checkpoint callback.
        //
        // see https://rust-lang.github.io/async-book/03_async_await/01_chapter.html#async-lifetimes for more information
        let allowed_ids_path = self.allowed_ids_path.clone();

        let handler = move |req: supergraph::Request| {
            // If we set a res, then we are going to break execution
            // If not, we are continuing
            let mut res = None;
            if !req.supergraph_request.headers().contains_key(&header_key) {
                // Prepare an HTTP 401 response with a GraphQL error message
                res = Some(
                    supergraph::Response::error_builder()
                        .error(
                            graphql::Error::builder()
                                .message(format!("Missing '{header_key}' header"))
                                .extension_code("AUTH_ERROR")
                                .build(),
                        )
                        .status_code(StatusCode::UNAUTHORIZED)
                        .context(req.context.clone())
                        .build()
                        .expect("response is valid"),
                );
            } else {
                // It is best practice to perform checks before we unwrap,
                // And to use `expect()` instead of `unwrap()`, with a message
                // that explains why the use of `expect()` is safe
                let client_id = req
                    .supergraph_request
                    .headers()
                    .get("x-client-id")
                    .expect("this cannot fail; we checked for header presence above;qed")
                    .to_str();

                match client_id {
                    Ok(client_id) => {
                        let allowed_clients: Vec<String> = serde_json::from_str(
                            std::fs::read_to_string(allowed_ids_path.clone())
                                .unwrap()
                                .as_str(),
                        )
                        .unwrap();

                        if !allowed_clients.contains(&client_id.to_string()) {
                            // Prepare an HTTP 403 response with a GraphQL error message
                            res = Some(
                                supergraph::Response::builder()
                                    .data(Value::default())
                                    .error(
                                        graphql::Error::builder()
                                            .message("client-id is not allowed")
                                            .extension_code("UNAUTHORIZED_CLIENT_ID")
                                            .build(),
                                    )
                                    .status_code(StatusCode::FORBIDDEN)
                                    .context(req.context.clone())
                                    .build()
                                    .expect("response is valid"),
                            );
                        }
                    }
                    Err(_not_a_string_error) => {
                        // Prepare an HTTP 400 response with a GraphQL error message
                        res = Some(
                            supergraph::Response::error_builder()
                                .error(
                                    graphql::Error::builder()
                                        .message(format!("'{header_key}' value is not a string"))
                                        .extension_code("BAD_CLIENT_ID")
                                        .build(),
                                )
                                .status_code(StatusCode::BAD_REQUEST)
                                .context(req.context.clone())
                                .build()
                                .expect("response is valid"),
                        );
                    }
                };
            }
            async {
                // Check to see if we built a response. If we did, we need to Break.
                match res {
                    Some(res) => Ok(ControlFlow::Break(res)),
                    None => Ok(ControlFlow::Continue(req)),
                }
            }
        };
        // `ServiceBuilder` provides us with an `oneshot_async_checkpoint` method.
        //
        // This method allows us to return ControlFlow::Continue(request) if we want to let the request through,
        // or ControlFlow::Break(response) with a crafted response if we don't want the request to go through.
        ServiceBuilder::new()
            .oneshot_checkpoint_async(handler)
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
    "allow_client_id_from_file",
    AllowClientIdFromFile
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
    use apollo_router::plugin::PluginInit;
    use apollo_router::services::supergraph;
    use apollo_router::TestHarness;
    use http::StatusCode;
    use serde_json::json;
    use tower::ServiceExt;

    use super::AllowClientIdFromFile;
    use crate::allow_client_id_from_file::AllowClientIdConfig;

    // This test ensures the router will be able to
    // find our `allow-client-id-from-file` plugin,
    // and deserialize an empty yml configuration containing a path
    // see router.yaml for more information
    #[tokio::test]
    async fn plugin_registered() {
        let config = json!({
            "plugins": {
                "example.allow_client_id_from_file": {
                    "header": "x-client-id",
                    "path": "allowedClientIds.json",
                }
            }
        });
        TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_no_client_id() {
        // create a mock service we will use to test our plugin
        // It does not have any behavior, because we do not expect it to be called.
        // If it is called, the test will panic,
        // letting us know AllowClientIdFromFile did not behave as expected.
        let mock_service = test::MockSupergraphService::new();

        // In this service_stack, AllowClientIdFromFile is `decorating` or `wrapping` our mock_service.
        let init = PluginInit::fake_builder()
            .config(AllowClientIdConfig {
                path: "allowedClientIds.json".to_string(),
                header: "x-client-id".to_string(),
            })
            .build();
        let service_stack = AllowClientIdFromFile::new(init)
            .await
            .expect("couldn't create AllowClientIdFromFile")
            .supergraph_service(mock_service.boxed());

        // Let's create a request without a client id...
        let request_without_client_id = supergraph::Request::fake_builder()
            .build()
            .expect("expecting valid request");

        // ...And call our service stack with it
        let mut service_response = service_stack
            .oneshot(request_without_client_id)
            .await
            .unwrap();

        // AllowClientIdFromFile should return a 401...
        assert_eq!(StatusCode::UNAUTHORIZED, service_response.response.status());

        // with the expected error message
        let graphql_response: graphql::Response = service_response.next_response().await.unwrap();

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
        let mock_service = test::MockSupergraphService::new();

        // In this service_stack, AllowClientIdFromFile is `decorating` or `wrapping` our mock_service.
        let init = PluginInit::fake_builder()
            .config(AllowClientIdConfig {
                path: "allowedClientIds.json".to_string(),
                header: "x-client-id".to_string(),
            })
            .build();
        let service_stack = AllowClientIdFromFile::new(init)
            .await
            .expect("couldn't create AllowClientIdFromFile")
            .supergraph_service(mock_service.boxed());

        // Let's create a request with a not allowed client id...
        let request_with_unauthorized_client_id = supergraph::Request::fake_builder()
            .header("x-client-id", "invalid_client_id")
            .build()
            .expect("expecting valid request");

        // ...And call our service stack with it
        let mut service_response = service_stack
            .oneshot(request_with_unauthorized_client_id)
            .await
            .unwrap();

        // AllowClientIdFromFile should return a 403...
        assert_eq!(StatusCode::FORBIDDEN, service_response.response.status());

        // with the expected error message
        let graphql_response: graphql::Response = service_response.next_response().await.unwrap();

        assert_eq!(
            "client-id is not allowed".to_string(),
            graphql_response.errors[0].message
        )
    }

    #[tokio::test]
    async fn test_client_id_allowed() {
        let valid_client_id = "jeremy";

        // create a mock service we will use to test our plugin
        let mut mock_service = test::MockSupergraphService::new();

        // The expected reply is going to be JSON returned in the SupergraphResponse { data } section.
        let expected_mock_response_data = "response created within the mock";

        // Let's set up our mock to make sure it will be called once, with the expected operation_name
        mock_service
            .expect_call()
            .times(1)
            .returning(move |req: supergraph::Request| {
                assert_eq!(
                    valid_client_id,
                    // we're ok with unwrap's here because we're running a test
                    // we would not do this in actual code
                    req.supergraph_request
                        .headers()
                        .get("x-client-id")
                        .unwrap()
                        .to_str()
                        .unwrap()
                );
                // let's return the expected data
                Ok(supergraph::Response::fake_builder()
                    .data(expected_mock_response_data)
                    .build()
                    .unwrap())
            });

        // In this service_stack, AllowClientIdFromFile is `decorating` or `wrapping` our mock_service.
        let init = PluginInit::fake_builder()
            .config(AllowClientIdConfig {
                path: "allowedClientIds.json".to_string(),
                header: "x-client-id".to_string(),
            })
            .build();
        let service_stack = AllowClientIdFromFile::new(init)
            .await
            .expect("couldn't create AllowClientIdFromFile")
            .supergraph_service(mock_service.boxed());

        // Let's create a request with an valid client id...
        let request_with_valid_client_id = supergraph::Request::fake_builder()
            .header("x-client-id", valid_client_id)
            .build()
            .expect("expecting valid request");

        // ...And call our service stack with it
        let mut service_response = service_stack
            .oneshot(request_with_valid_client_id)
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
