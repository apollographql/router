//! % curl -v \
//!    --request POST \
//!    --header 'content-type: application/json' \
//!    --url 'http://127.0.0.1:4000' \
//!    --data '{"query":"query Me {\n  me {\n    name\n  }\n}"}'

use anyhow::Result;

// `cargo run -- -s ../graphql/supergraph.graphql -c ./router.yaml`
fn main() -> Result<()> {
    apollo_router::main()
}

#[cfg(test)]
mod tests {
    use apollo_router::plugin::test;
    use apollo_router::plugin::Plugin;
    use apollo_router::plugin::PluginInit;
    use apollo_router::plugins::rhai::Conf;
    use apollo_router::plugins::rhai::Rhai;
    use apollo_router::stages::router;
    use http::StatusCode;
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn test_subgraph_processes_operation_name() {
        // create a mock service we will use to test our plugin
        let mut mock_service = test::MockRouterService::new();

        // The expected reply is going to be JSON returned in the RouterResponse { data } section.
        let expected_mock_response_data = "response created within the mock";

        // Let's set up our mock to make sure it will be called once
        mock_service
            .expect_call()
            .once()
            .returning(move |_req: router::Request| {
                Ok(router::Response::fake_builder()
                    .data(expected_mock_response_data)
                    .build()
                    .unwrap())
            });

        let conf: Conf = serde_json::from_value(serde_json::json!({
            "scripts": "src",
            "main": "rhai_logging.rhai",
        }))
        .expect("valid conf supplied");

        // Build an instance of our plugin to use in the test harness
        let rhai = Rhai::new(PluginInit::new(conf, Default::default()))
            .await
            .expect("created plugin");

        let service_stack = rhai.router_service(mock_service.boxed());

        // Let's create a request with our operation name
        let request_with_appropriate_name = router::Request::fake_builder()
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

        // with the expected message
        let response = service_response.next_response().await.unwrap();
        assert!(response.errors.is_empty());
        assert_eq!(expected_mock_response_data, response.data.as_ref().unwrap());
    }
}
