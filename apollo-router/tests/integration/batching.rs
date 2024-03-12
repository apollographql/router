use apollo_router::graphql::Request;
use itertools::Itertools;
use tower::BoxError;
use wiremock::ResponseTemplate;

#[tokio::test(flavor = "multi_thread")]
async fn it_supports_single_subgraph_batching() -> Result<(), BoxError> {
    const REQUEST_COUNT: usize = 3;

    fn expect_batch(request: &wiremock::Request) -> ResponseTemplate {
        let requests: Vec<Request> = request.body_json().unwrap();

        // We should have gotten REQUEST_COUNT elements
        assert_eq!(requests.len(), REQUEST_COUNT);

        // Each element should have be for entryA and should have a field selection
        // of index.
        // Note: The router appends info to the query, so we append it at this check
        for (index, request) in requests.into_iter().enumerate() {
            assert_eq!(
                request.query,
                Some(format!("query op{index}__a__0{{entryA{{index}}}}"))
            );
        }

        ResponseTemplate::new(200).set_body_json(
            (0..REQUEST_COUNT)
                .map(|index| {
                    serde_json::json!({
                        "data": {
                            "entryA": {
                                "index": index
                            }
                        }
                    })
                })
                .collect::<Vec<_>>(),
        )
    }

    let requests: Vec<_> = (0..REQUEST_COUNT)
        .map(|index| {
            Request::fake_builder()
                .query(format!("query op{index}{{ entryA {{ index }} }}"))
                .build()
        })
        .collect();
    let responses = helper::run_test(&requests[..], Some(expect_batch), None).await?;

    // Make sure that we got back what we wanted
    for (index, response) in responses.into_iter().enumerate() {
        assert_eq!(response.errors, Vec::new());
        assert_eq!(
            response.data,
            Some(serde_json_bytes::json!({
                "entryA": {
                    "index": index
                }
            }))
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn it_supports_multi_subgraph_batching() -> Result<(), BoxError> {
    const REQUEST_COUNT: usize = 3;

    fn expect_batch(request: &wiremock::Request) -> ResponseTemplate {
        let requests: Vec<Request> = request.body_json().unwrap();

        // We should have gotten REQUEST_COUNT elements
        assert_eq!(requests.len(), REQUEST_COUNT);

        // See which subgraph we're in
        let subgraph = {
            let re = regex::Regex::new("entry([AB])").unwrap();
            let captures = re.captures(requests[0].query.as_ref().unwrap()).unwrap();

            captures[1].to_string()
        };

        // Each element should have be for entryA and should have a field selection
        // of index.
        // Note: The router appends info to the query, so we append it at this check
        for (index, request) in requests.into_iter().enumerate() {
            assert_eq!(
                request.query,
                Some(format!(
                    "query op{index}__{}__0{{entry{}{{index}}}}",
                    subgraph.to_lowercase(),
                    subgraph
                ))
            );
        }

        ResponseTemplate::new(200).set_body_json(
            (0..REQUEST_COUNT)
                .map(|index| {
                    serde_json::json!({
                        "data": {
                            format!("entry{subgraph}"): {
                                "index": index
                            }
                        }
                    })
                })
                .collect::<Vec<_>>(),
        )
    }

    let requests_a = (0..REQUEST_COUNT).map(|index| {
        Request::fake_builder()
            .query(format!("query op{index}{{ entryA {{ index }} }}"))
            .build()
    });
    let requests_b = (0..REQUEST_COUNT).map(|index| {
        Request::fake_builder()
            .query(format!("query op{index}{{ entryB {{ index }} }}"))
            .build()
    });

    // Interleave requests so that we can verify that they get properly separated
    let requests: Vec<_> = requests_a.interleave(requests_b).collect();

    let responses = helper::run_test(&requests, Some(expect_batch), Some(expect_batch)).await?;

    // Make sure that we got back what we wanted
    for (index, response) in responses.into_iter().enumerate() {
        // Responses interleaved
        let subgraph = if index % 2 == 0 { "A" } else { "B" };
        let index = index / 2;

        assert_eq!(response.errors, Vec::new());
        assert_eq!(
            response.data,
            Some(serde_json_bytes::json!({
                format!("entry{subgraph}"): {
                    "index": index
                }
            }))
        );
    }

    Ok(())
}

/// Utility methods for these tests
mod helper {
    use apollo_router::graphql::Request;
    use apollo_router::graphql::Response;
    use tower::BoxError;
    use wiremock::matchers;
    use wiremock::MockServer;
    use wiremock::Respond;
    use wiremock::ResponseTemplate;

    use crate::integration::common::IntegrationTest;

    const CONFIG: &str = include_str!("../fixtures/batching/all_enabled.router.yaml");

    /// Helper method for creating a wiremock handler from a handler
    ///
    /// If the handler is `None`, then the fallback is to always fail any request to the mock server
    macro_rules! make_handler {
        ($subgraph_path:expr, $handler:expr) => {
            if let Some(f) = $handler {
                wiremock::Mock::given(matchers::method("POST"))
                    .and(matchers::path($subgraph_path))
                    .respond_with(f)
                    .expect(1)
                    .named(stringify!(batching POST $subgraph_path))
            } else {
                wiremock::Mock::given(matchers::method("POST"))
                    .and(matchers::path($subgraph_path))
                    .respond_with(always_fail)
                    .expect(0)
                    .named(stringify!(batching POST $subgraph_path))
            }
        }
    }

    /// Set up the integration test stack
    pub async fn run_test<F: Respond + 'static>(
        requests: &[Request],
        handler_a: Option<F>,
        handler_b: Option<F>,
    ) -> Result<Vec<Response>, BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if std::env::var("TEST_APOLLO_KEY").is_err()
            || std::env::var("TEST_APOLLO_GRAPH_REF").is_err()
        {
            return Ok(Vec::new());
        };

        // Create a wiremock server for each handler
        let mock_server = MockServer::start().await;
        mock_server.register(make_handler!("/a", handler_a)).await;
        mock_server.register(make_handler!("/b", handler_b)).await;

        // Start up the router with the mocked subgraphs
        let mut router = IntegrationTest::builder()
            .config(CONFIG)
            .supergraph("tests/fixtures/batching/schema.graphql")
            .subgraph_override("a", format!("{}/a", mock_server.uri()))
            .subgraph_override("b", format!("{}/b", mock_server.uri()))
            .build()
            .await;

        router.start().await;
        router.assert_started().await;

        // Execute the request
        let request = serde_json::to_value(requests)?;
        let (_span, response) = router.execute_query(&request).await;

        serde_json::from_slice::<Vec<Response>>(&response.bytes().await?).map_err(BoxError::from)
    }

    /// Subgraph handler that always fails
    ///
    /// Useful for subgraphs tests that should never actually be called
    fn always_fail(_request: &wiremock::Request) -> ResponseTemplate {
        ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "errors": [{
                "message": "called into subgraph that should not have happened",
            }]
        }))
    }
}
