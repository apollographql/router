//! garypen@Garys-MBP router % curl -v \
//!    --header 'content-type: application/json' \
//!    --cookie 'yummy_cookie=choco' \
//!    --cookie 'tasty_cookie=strawberry' \
//!    --url 'http://127.0.0.1:4000' \
//!    --data '{"query":"query Query {\n  me {\n    name\n  }\n}"}'
//!    Trying 127.0.0.1:4000...
//!  Connected to 127.0.0.1 (127.0.0.1) port 4000 (#0)
//!  POST / HTTP/1.1
//!  Host: 127.0.0.1:4000
//!  User-Agent: curl/7.79.1
//!  Accept: */*
//!  Cookie: yummy_cookie=choco;tasty_cookie=strawberry
//!  content-type: application/json
//!  Content-Length: 51
//!  
//!  Mark bundle as not supporting multiuse
//!  HTTP/1.1 200 OK
//!  content-type: application/json
//!  content-length: 39
//!  date: Thu, 17 Mar 2022 09:53:55 GMT
//!  
//!  Connection #0 to host 127.0.0.1 left intact
//! "data":{"me":{"name":"Ada Lovelace"}}}%

use anyhow::Result;

// `cargo run -- -s ../graphql/supergraph.graphql -c ./router.yaml`
fn main() -> Result<()> {
    apollo_router::main()
}

#[cfg(test)]
mod tests {
    use apollo_router::plugins::rhai::{Conf, Rhai};
    use apollo_router_core::{
        http_compat, plugin::utils, Plugin, SubgraphRequest, SubgraphResponse,
    };
    use http::{header::HeaderName, HeaderValue, StatusCode};
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn test_subgraph_processes_cookies() {
        // create a mock service we will use to test our plugin
        let mut mock = utils::test::MockSubgraphService::new();

        // The expected reply is going to be JSON returned in the SubgraphResponse { data } section.
        let expected_mock_response_data = "response created within the mock";

        // Let's set up our mock to make sure it will be called once
        mock.expect_call()
            .once()
            .returning(move |req: SubgraphRequest| {
                // Let's make sure our request contains our new headers
                assert_eq!(
                    req.subgraph_request
                        .headers()
                        .get("yummy_cookie")
                        .expect("yummy_cookie is present"),
                    "choco"
                );
                assert_eq!(
                    req.subgraph_request
                        .headers()
                        .get("tasty_cookie")
                        .expect("tasty_cookie is present"),
                    "strawberry"
                );
                Ok(SubgraphResponse::fake_builder()
                    .data(expected_mock_response_data)
                    .build())
            });

        // The mock has been set up, we can now build a service from it
        let mock_service = mock.build();

        let conf: Conf = serde_json::from_value(serde_json::json!({
            "filename": "src/cookies_to_headers.rhai",
        }))
        .expect("json must be valid");

        // In this service_stack, JwtAuth is `decorating` or `wrapping` our mock_service.
        let mut rhai = Rhai::new(conf)
            .await
            .expect("valid configuration should succeed");

        let service_stack = rhai.subgraph_service("mock", mock_service.boxed());

        let mut sub_request = http_compat::Request::mock();

        let headers = vec![(
            HeaderName::from_static("cookie"),
            HeaderValue::from_static("yummy_cookie=choco;tasty_cookie=strawberry"),
        )];

        for (name, value) in headers {
            sub_request.headers_mut().insert(name, value);
        }

        // Let's create a request with our cookies
        let request_with_appropriate_cookies = SubgraphRequest::fake_builder()
            .subgraph_request(sub_request)
            .build();

        // ...And call our service stack with it
        let service_response = service_stack
            .oneshot(request_with_appropriate_cookies)
            .await
            .unwrap();

        // Rhai should return a 200...
        assert_eq!(StatusCode::OK, service_response.response.status());

        // with the expected message
        let graphql_response: apollo_router_core::Response = service_response.response.into_body();

        assert!(graphql_response.errors.is_empty());
        assert_eq!(expected_mock_response_data, graphql_response.data.unwrap())
    }
}
