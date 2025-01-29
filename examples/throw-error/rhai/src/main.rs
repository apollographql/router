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
    use apollo_router::services::supergraph;
    use apollo_router::Context;
    use http::StatusCode;
    use http_body_util::BodyExt;
    use serde_json::json;
    use tower::ServiceExt;

    #[tokio::test]
    async fn test_subgraph_graphql_error_error() {
        let config = serde_json::json!({
            "rhai": {
                "scripts": "src",
                "main": "rhai_throw_error.rhai",
            }
        });
        let test_harness = apollo_router::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .unwrap();

        // The expected reply is going to be JSON returned in the SupergraphResponse { error } section.
        let expected_response = json!({
            "errors": [{
                "message": "Access denied",
                "extensions": {
                    "code": "UNAUTHENTICATED"
                }
            }]
        });

        // ... Call our test harness
        let query = "query {topProducts{name}}";
        let operation_name: Option<&str> = None;
        let context: Option<Context> = None;
        let service_response = test_harness
            .oneshot(
                supergraph::Request::fake_builder()
                    .header("name_header", "test_client")
                    .header("version_header", "1.0-test")
                    .query(query)
                    .and_operation_name(operation_name)
                    .and_context(context)
                    .build()
                    .expect("a valid SupergraphRequest")
                    .try_into()
                    .unwrap(),
            )
            .await
            .expect("a router response");

        assert_eq!(StatusCode::UNAUTHORIZED, service_response.response.status());
        let body = service_response
            .response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        assert_eq!(
            expected_response,
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()
        );
    }
}
