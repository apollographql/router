//! garypen@Garys-MBP router % curl -v \
//!    --header 'content-type: application/json' \
//!    --url 'http://127.0.0.1:4000' \
//!    --data '{"operationName": "me", "query":"query Query {\n  me {\n    name\n  }\n}"}'

use anyhow::Result;

// `cargo run -- -s ../graphql/supergraph.graphql -c ./router.yaml`
fn main() -> Result<()> {
    apollo_router::main()
}

#[cfg(test)]
mod tests {
    use apollo_router::plugins::rhai::{Conf, Rhai};
    use apollo_router_core::{plugin::utils, Plugin, RouterRequest, RouterResponse};
    use futures::{stream::once, StreamExt};
    use http::StatusCode;
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn test_subgraph_processes_operation_name() {
        // create a mock service we will use to test our plugin
        let mut mock = utils::test::MockRouterService::new();

        // The expected reply is going to be JSON returned in the RouterResponse { data } section.
        let expected_mock_response_data = "response created within the mock";

        // Let's set up our mock to make sure it will be called once
        mock.expect_call()
            .once()
            .returning(move |req: RouterRequest| {
                // Let's make sure our request contains our new header
                assert_eq!(
                    req.originating_request
                        .headers()
                        .get("X-operation-name")
                        .expect("X-operation-name is present"),
                    "me"
                );
                Ok(Box::pin(once(async move {
                    RouterResponse::fake_builder()
                        .data(expected_mock_response_data)
                        .build()
                        .unwrap()
                })))
            });

        // The mock has been set up, we can now build a service from it
        let mock_service = mock.build();

        let conf: Conf = serde_json::from_value(serde_json::json!({
            "filename": "src/op_name_to_header.rhai",
        }))
        .expect("json must be valid");

        // Build a rhai plugin instance from our conf
        let mut rhai = Rhai::new(conf)
            .await
            .expect("valid configuration should succeed");

        let service_stack = rhai.router_service(mock_service.boxed());

        // Let's create a request with our operation name
        let request_with_appropriate_name = RouterRequest::fake_builder()
            .operation_name("me".to_string())
            .build()
            .unwrap();

        // ...And call our service stack with it
        let service_response = service_stack
            .oneshot(request_with_appropriate_name)
            .await
            .unwrap()
            .next()
            .await
            .unwrap();

        // Rhai should return a 200...
        assert_eq!(StatusCode::OK, service_response.response.status());

        // with the expected message
        if let apollo_router_core::ResponseBody::GraphQL(response) =
            service_response.response.body()
        {
            assert!(response.errors.is_empty());
            assert_eq!(expected_mock_response_data, response.data.as_ref().unwrap());
        } else {
            panic!("unexpected response");
        }
    }
}
