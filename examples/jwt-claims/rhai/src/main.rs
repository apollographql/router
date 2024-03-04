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

// `cargo run -- -s ../../graphql/supergraph.graphql -c ./router.yaml`
fn main() -> Result<()> {
    apollo_router::main()
}

#[cfg(test)]
mod tests {

    use apollo_router::graphql;
    use apollo_router::plugin::test;
    use apollo_router::services::router;
    use apollo_router::services::supergraph;
    use http::StatusCode;
    use tower::util::ServiceExt;

    async fn build_a_test_harness() -> router::BoxCloneService {
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
                    Ok(supergraph::Response::fake_builder()
                        .data(expected_mock_response_data)
                        .context(req.context)
                        .build()
                        .unwrap())
                });
            mock_service
        });

        #[cfg(target_os = "windows")]
        let mut jwks_file = std::fs::canonicalize("jwks.json").unwrap();
        #[cfg(not(target_os = "windows"))]
        let jwks_file = std::fs::canonicalize("jwks.json").unwrap();

        #[cfg(target_os = "windows")]
        {
            // We need to manipulate our canonicalized file if we are on Windows.
            // We replace windows path separators with posix path separators
            // We also drop the first 3 characters from the path since they will be
            // something like (drive letter may vary) '\\?\C:' and that isn't
            // a valid URI
            let mut file_string = jwks_file.display().to_string();
            file_string = file_string.replace("\\", "/");
            let len = file_string
                .char_indices()
                .nth(3)
                .map_or(0, |(idx, _ch)| idx);
            jwks_file = file_string[len..].into();
        }

        let jwks_url = format!("file://{}", jwks_file.display());
        let config = serde_json::json!({
            "authentication": {
                "router": {
                    "jwt": {
                        "jwks": [
                            {
                                "url": &jwks_url
                            }
                        ]
                    }
                }
            }
        });

        apollo_router::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .supergraph_hook(move |_| mock_service.clone().boxed())
            .build_router()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn load_plugin() {
        let _test_harness = build_a_test_harness().await;
    }

    #[tokio::test]
    async fn it_accepts_when_auth_prefix_has_correct_format_and_valid_jwt() {
        let test_harness = build_a_test_harness().await;

        // Let's create a request with our operation name
        let request_with_appropriate_name = supergraph::Request::canned_builder()
            .operation_name("me".to_string())
            .header(
                http::header::AUTHORIZATION,
                "Bearer eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiIsImtpZCI6ImtleTEifQ.eyJleHAiOjEwMDAwMDAwMDAwLCJhbm90aGVyIGNsYWltIjoidGhpcyBpcyBhbm90aGVyIGNsYWltIn0.4GrmfxuUST96cs0YUC0DfLAG218m7vn8fO_ENfXnu5A",
            )
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

        assert_eq!(response.errors, vec![]);

        assert_eq!(StatusCode::OK, service_response.response.status());

        let expected_mock_response_data = "response created within the mock";
        // with the expected message
        assert_eq!(expected_mock_response_data, response.data.as_ref().unwrap());
    }
}
