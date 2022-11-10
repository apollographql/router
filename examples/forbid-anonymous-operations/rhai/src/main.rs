//! % curl -v \
//!    --header 'content-type: application/json' \
//!    --url 'http://127.0.0.1:4000' \
//!    --data '{"operationName": "", "query":"query Query {\n  me {\n    name\n  }\n}"}'

use anyhow::Result;

// `cargo run -- -s ../../graphql/supergraph.graphql -c ./router.yaml`
fn main() -> Result<()> {
    apollo_router::main()
}

#[cfg(test)]
mod tests {
    use apollo_router::plugin::test;
    use apollo_router::services::supergraph;
    use http::StatusCode;
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn test_router_forbids_anonymous_operation() {
        let mut mock_service = test::MockSupergraphService::new();
        // create a mock service we will use to test our plugin

        // The expected reply is going to be JSON returned in the SupergraphResponse { data } section.
        let expected_mock_response_data = "response created within the mock";

        // Let's set up our mock to make sure it will be called once
        mock_service.expect_clone().return_once(move || {
            let mut mock_service = test::MockSupergraphService::new();
            mock_service
                .expect_call()
                .once()
                .returning(move |req: supergraph::Request| {
                    // Preserve our context from request to response
                    Ok(supergraph::Response::fake_builder()
                        .context(req.context)
                        .data(expected_mock_response_data)
                        .build()
                        .unwrap())
                });
            mock_service
        });

        let config = serde_json::json!({
            "rhai": {
                "scripts": "src",
                "main": "forbid_anonymous_operations.rhai",
            }
        });
        let test_harness = apollo_router::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .supergraph_hook(move |_| mock_service.clone().boxed())
            .build()
            .await
            .unwrap();

        // Let's create a request with our operation name
        let request_with_no_name = supergraph::Request::canned_builder().build().unwrap();

        // ...And call our service stack with it
        let mut service_response = test_harness.oneshot(request_with_no_name).await.unwrap();

        let _response = service_response.next_response().await.unwrap();
        println!("RESPONSE: {:?}", _response);
        // Rhai should return a 500
        assert_eq!(
            StatusCode::INTERNAL_SERVER_ERROR,
            service_response.response.status()
        );
    }
}
