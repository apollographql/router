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
    use apollo_router::services::supergraph;
    use http::StatusCode;
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn test_router_service_adds_timestamp_header() {
        let expected_mock_response_data = "response created within the mock";
        let (mock_service, mut handle) =
            tower_test::mock::pair::<supergraph::Request, supergraph::Response>();
        tokio::spawn(async move {
            let (req, responder) = handle.next_request().await.unwrap();
            responder.send_response(
                supergraph::Response::fake_builder()
                    .context(req.context)
                    .data(expected_mock_response_data)
                    .build()
                    .unwrap(),
            );
        });

        let config = serde_json::json!({
            "rhai": {
                "scripts": "src",
                "main": "add_timestamp_header.rhai",
            }
        });
        let test_harness = apollo_router::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .supergraph_hook(move |_| mock_service.clone().boxed_clone())
            .build_router()
            .await
            .unwrap();

        // Let's create a request with our operation name
        let request_with_appropriate_name = supergraph::Request::canned_builder()
            .operation_name("TopProducts".to_string())
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

        // with the expected header
        service_response
            .response
            .headers()
            .get("x-elapsed-time")
            .expect("x-elapsed-time is present");

        // with the expected message
        assert_eq!(expected_mock_response_data, response.data.as_ref().unwrap());
    }
}
