//! % curl -v \
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
    use apollo_router::plugin::test;
    use apollo_router::stages::router;
    use apollo_router::stages::subgraph;
    use futures::stream::StreamExt;
    use http::StatusCode;
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn test_subgraph_processes_cookies() {
        // create a mock service we will use to test our plugin
        let mut mock_service = test::MockSubgraphService::new();

        // The expected reply is going to be JSON returned in the SubgraphResponse { data } section.
        let expected_mock_response_data = "response created within the mock";

        // Let's set up our mock to make sure it will be called once
        mock_service.expect_clone().return_once(move || {
            let mut mock_service = test::MockSubgraphService::new();
            mock_service
                .expect_call()
                .once()
                .returning(move |req: subgraph::Request| {
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
                    req.context
                        .insert("mock_data", expected_mock_response_data.to_owned())
                        .unwrap();
                    Ok(subgraph::Response::fake_builder().build())
                });
            mock_service
        });

        let config = serde_json::json!({
            "rhai": {
                "scripts": "src",
                "main": "cookies_to_headers.rhai",
            }
        });
        let test_harness = apollo_router::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .extra_subgraph_plugin(move |_, _| mock_service.clone().boxed())
            .extra_router_plugin(|service| {
                service
                    .map_response(|mut response| {
                        let mock_data = response.context.get("mock_data").unwrap();
                        let body = response.response.body_mut();
                        let dummy = futures::stream::empty().boxed();
                        let stream = std::mem::replace(body, dummy);
                        *body = stream
                            .map(move |mut resp| {
                                resp.data = mock_data.clone();
                                resp
                            })
                            .boxed();
                        response
                    })
                    .boxed()
            })
            .build()
            .await
            .unwrap();

        let request_with_appropriate_cookies = router::Request::canned_builder()
            .header("cookie", "yummy_cookie=choco;tasty_cookie=strawberry")
            .build()
            .unwrap();

        // ...And call our service stack with it
        let mut service_response = test_harness
            .oneshot(request_with_appropriate_cookies)
            .await
            .unwrap();

        let response = service_response.next_response().await.unwrap();
        assert_eq!(response.errors, []);
        // Rhai should return a 200...
        assert_eq!(StatusCode::OK, service_response.response.status());

        // with the expected message
        assert_eq!(expected_mock_response_data, response.data.unwrap())
    }
}
