//! % curl -v \
//!    --header 'content-type: application/json' \
//!    --url 'http://127.0.0.1:4000' \
//!    --data '{"operationName": "me", "query":"query Query {\n  me {\n    name\n  }\n}"}'

use anyhow::Result;

// `cargo run -- -s ../../graphql/supergraph.graphql -c ./router.yaml`
fn main() -> Result<()> {
    apollo_router::main()
}

#[cfg(test)]
mod tests {
    use apollo_router::graphql;
    use apollo_router::plugin::test;
    use apollo_router::services::supergraph;
    use http::StatusCode;
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn test_subgraph_processes_operation_name() {
        // create a mock service we will use to test our plugin
        let mut mock_service = test::MockSupergraphService::new();

        // The expected reply is going to be JSON returned in the SupergraphResponse { data } section.
        let expected_mock_response_data = "response created within the mock";

        // Let's set up our mock to make sure it will be called once
        mock_service.expect_clone().return_once(move || {
            let mut mock_service = test::MockSupergraphService::new();
            mock_service
                .expect_call()
                .once()
                .returning(move |req: supergraph::Request| {
                    // Let's make sure our request contains our new header
                    assert_eq!(
                        req.supergraph_request
                            .headers()
                            .get("X-operation-name")
                            .expect("X-operation-name is present"),
                        "me"
                    );
                    Ok(supergraph::Response::fake_builder()
                        .data(expected_mock_response_data)
                        .context(req.context)
                        .build()
                        .unwrap())
                });
            mock_service
        });

        let config = serde_json::json!({
            "rhai": {
                "scripts": "src",
                "main": "op_name_to_header.rhai",
            }
        });
        let test_harness = apollo_router::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .supergraph_hook(move |_| mock_service.clone().boxed())
            .build_router()
            .await
            .unwrap();

        // Let's create a request with our operation name
        let request_with_appropriate_name = supergraph::Request::canned_builder()
            .operation_name("me".to_string())
            .build()
            .unwrap();

        // ...And call our service stack with it
        let mut service_response = test_harness
            .oneshot(request_with_appropriate_name.try_into().unwrap())
            .await
            .unwrap();
        let response: graphql::Response = serde_json::from_slice(
            service_response
                .next_response()
                .await
                .unwrap()
                .unwrap()
                .to_vec()
                .as_slice(),
        )
        .unwrap();
        assert_eq!(response.errors, []);

        // Rhai should return a 200...
        assert_eq!(StatusCode::OK, service_response.response.status());

        // with the expected message
        assert_eq!(expected_mock_response_data, response.data.as_ref().unwrap());
    }
}
