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
    use apollo_router_core::{plugin::utils, Plugin, RouterRequest};
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
                    req.context
                        .request
                        .headers()
                        .get("X-operation-name")
                        .expect("X-operation-name is present"),
                    "me"
                );
                Ok(utils::RouterResponse::builder()
                    .data(expected_mock_response_data.into())
                    .build()
                    .into())
            });

        // The mock has been set up, we can now build a service from it
        let mock_service = mock.build();

        let conf: Conf = serde_json::from_value(serde_json::json!({
            "filename": "src/op_name_to_header.rhai",
        }))
        .expect("json must be valid");

        // In this service_stack, JwtAuth is `decorating` or `wrapping` our mock_service.
        let mut rhai = Rhai::new(conf).expect("valid configuration should succeed");

        let service_stack = rhai.router_service(mock_service.boxed());

        // Let's create a request with our operation name
        let request_with_appropriate_name = utils::RouterRequest::builder()
            .operation_name("me".to_string())
            .build()
            .into();

        // ...And call our service stack with it
        let service_response = service_stack
            .oneshot(request_with_appropriate_name)
            .await
            .unwrap();

        // Rhai should return a 200...
        assert_eq!(StatusCode::OK, service_response.response.status());

        // with the expected message
        if let apollo_router_core::ResponseBody::GraphQL(response) =
            service_response.response.body()
        {
            assert!(response.errors.is_empty());
            assert_eq!(expected_mock_response_data, response.data);
        } else {
            panic!("unexpected response");
        }
    }
}
