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
    use apollo_router::services::supergraph;
    use http::StatusCode;
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn test_router_forbids_anonymous_operation() {
        let (mock_service, mut handle) =
            tower_test::mock::pair::<supergraph::Request, supergraph::Response>();

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
            .build_router()
            .await
            .unwrap();

        // Let's create a request with our operation name
        let request_with_no_name = supergraph::Request::canned_builder().build().unwrap();

        // ...And call our service stack with it
        let mut service_response = test_harness
            .oneshot(request_with_no_name.try_into().unwrap())
            .await
            .unwrap();

        let _response = service_response.next_response().await.unwrap();
        println!("RESPONSE: {_response:?}");
        // Rhai should return a 500
        assert_eq!(
            StatusCode::INTERNAL_SERVER_ERROR,
            service_response.response.status()
        );
        if matches!(
            tokio::time::timeout(std::time::Duration::from_millis(10), handle.next_request()).await,
            Ok(Some(_))
        ) {
            panic!("mock service was called but should not have been");
        }
    }
}
