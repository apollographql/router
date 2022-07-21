//! % curl -v \
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
    use apollo_router::plugin::test;
    use apollo_router::plugin::Plugin;
    use apollo_router::plugins::rhai::Conf;
    use apollo_router::plugins::rhai::Rhai;
    use apollo_router::services::RouterRequest;
    use apollo_router::services::RouterResponse;
    use http::StatusCode;
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn test_router_service_adds_timestamp_header() {
        // create a mock service we will use to test our plugin
        let mut mock = test::MockRouterService::new();

        // The expected reply is going to be JSON returned in the RouterResponse { data } section.
        let expected_mock_response_data = "response created within the mock";

        // Let's set up our mock to make sure it will be called once
        mock.expect_call()
            .once()
            .returning(move |req: RouterRequest| {
                // Preserve our context from request to response
                Ok(RouterResponse::fake_builder()
                    .context(req.context)
                    .data(expected_mock_response_data)
                    .build()
                    .unwrap())
            });

        // The mock has been set up, we can now build a service from it
        let mock_service = mock.build();

        let conf: Conf = serde_json::from_value(serde_json::json!({
            "scripts": "src",
            "main": "add_timestamp_header.rhai",
        }))
        .expect("json must be valid");

        // Build a rhai plugin instance from our conf
        let rhai = Rhai::new(conf)
            .await
            .expect("valid configuration should succeed");

        let service_stack = rhai.router_service(mock_service.boxed());

        // Let's create a request with our operation name
        let request_with_appropriate_name = RouterRequest::fake_builder()
            .operation_name("me".to_string())
            .build()
            .unwrap();

        // ...And call our service stack with it
        let mut service_response = service_stack
            .oneshot(request_with_appropriate_name)
            .await
            .unwrap();

        // Rhai should return a 200...
        assert_eq!(StatusCode::OK, service_response.response.status());

        // with the expected header
        service_response
            .response
            .headers()
            .get("x-elapsed-time")
            .expect("x-elapsed-time is present");

        // with the expected message
        let response = service_response.next_response().await.unwrap();
        assert!(response.errors.is_empty());
        assert_eq!(expected_mock_response_data, response.data.as_ref().unwrap());
    }
}
