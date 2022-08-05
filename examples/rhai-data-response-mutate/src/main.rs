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
    use apollo_router::plugin::test::PluginTestHarness;
    use apollo_router::services::RouterRequest;
    use apollo_router::Context;
    use http::StatusCode;

    #[tokio::test]
    async fn test_subgraph_mutates_data() {
        // Define a configuration to use our plugin
        let conf = serde_json::json!({
            "rhai": {
                "scripts": "src",
                "main": "rhai_data_response_mutate.rhai",
            }
        });
        // Build a test harness.
        let mut test_harness = PluginTestHarness::builder()
            .configuration(conf)
            .build()
            .await
            .expect("building harness");

        // The expected reply is going to be JSON returned in the RouterResponse { data } section.
        let _expected_mock_response_data = "response created within the mock";

        // ... Call our test harness
        let query = "query {topProducts{name}}";
        let operation_name: Option<&str> = None;
        let context: Option<Context> = None;
        let mut service_response = test_harness
            .call(
                RouterRequest::fake_builder()
                    .header("name_header", "test_client")
                    .header("version_header", "1.0-test")
                    .query(query)
                    .and_operation_name(operation_name)
                    .and_context(context)
                    .build()
                    .expect("a valid RouterRequest"),
            )
            .await
            .expect("a router response");

        assert_eq!(
            StatusCode::INTERNAL_SERVER_ERROR,
            service_response.response.status()
        );
        let _response_body = service_response.next_response().await.unwrap();
        /* TBD: Figure out how to run this as a test
        // Rhai should return a 200...
        println!("RESPONSE: {:?}", service_response);
        assert_eq!(StatusCode::OK, service_response.response.status());

        // with the expected message
        let response = service_response.response.body();
        assert!(response.errors.is_empty());
        assert_eq!(expected_mock_response_data, response.data.as_ref().unwrap());
        */
    }
}
