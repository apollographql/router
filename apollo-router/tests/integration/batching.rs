use apollo_router::graphql::Request;
use insta::assert_yaml_snapshot;
use itertools::Itertools;
use tower::BoxError;

const CONFIG: &str = include_str!("../fixtures/batching/all_enabled.router.yaml");
const SHORT_TIMEOUTS_CONFIG: &str = include_str!("../fixtures/batching/short_timeouts.router.yaml");

#[tokio::test(flavor = "multi_thread")]
async fn it_supports_single_subgraph_batching() -> Result<(), BoxError> {
    const REQUEST_COUNT: usize = 5;

    let requests: Vec<_> = (0..REQUEST_COUNT)
        .map(|index| {
            Request::fake_builder()
                .query(format!(
                    "query op{index}{{ entryA(count: {REQUEST_COUNT}) {{ index }} }}"
                ))
                .build()
        })
        .collect();
    let responses = helper::run_test(
        CONFIG,
        &requests[..],
        Some(helper::expect_batch),
        None::<helper::Handler>,
    )
    .await?;

    // Make sure that we got back what we wanted
    assert_yaml_snapshot!(responses, @r###"
    ---
    - data:
        entryA:
          index: 0
    - data:
        entryA:
          index: 1
    - data:
        entryA:
          index: 2
    - data:
        entryA:
          index: 3
    - data:
        entryA:
          index: 4
    "###);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn it_supports_multi_subgraph_batching() -> Result<(), BoxError> {
    const REQUEST_COUNT: usize = 3;

    let requests_a = (0..REQUEST_COUNT).map(|index| {
        Request::fake_builder()
            .query(format!(
                "query op{index}{{ entryA(count: {REQUEST_COUNT}) {{ index }} }}"
            ))
            .build()
    });
    let requests_b = (0..REQUEST_COUNT).map(|index| {
        Request::fake_builder()
            .query(format!(
                "query op{index}{{ entryB(count: {REQUEST_COUNT}) {{ index }} }}"
            ))
            .build()
    });

    // Interleave requests so that we can verify that they get properly separated
    let requests: Vec<_> = requests_a.interleave(requests_b).collect();
    let responses = helper::run_test(
        CONFIG,
        &requests,
        Some(helper::expect_batch),
        Some(helper::expect_batch),
    )
    .await?;

    // Make sure that we got back what we wanted
    assert_yaml_snapshot!(responses, @r###"
    ---
    - data:
        entryA:
          index: 0
    - data:
        entryB:
          index: 0
    - data:
        entryA:
          index: 1
    - data:
        entryB:
          index: 1
    - data:
        entryA:
          index: 2
    - data:
        entryB:
          index: 2
    "###);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn it_batches_with_errors_in_single_graph() -> Result<(), BoxError> {
    const REQUEST_COUNT: usize = 4;

    let requests: Vec<_> = (0..REQUEST_COUNT)
        .map(|index| {
            Request::fake_builder()
                .query(format!(
                    "query op{index}{{ entryA(count: {REQUEST_COUNT}) {{ index }} }}"
                ))
                .build()
        })
        .collect();
    let responses = helper::run_test(
        CONFIG,
        &requests[..],
        Some(helper::fail_second_batch_request),
        None::<helper::Handler>,
    )
    .await?;

    // Make sure that we got back what we wanted
    assert_yaml_snapshot!(responses, @r###"
    ---
    - data:
        entryA:
          index: 0
    - errors:
        - message: expected error in A
    - data:
        entryA:
          index: 2
    - data:
        entryA:
          index: 3
    "###);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn it_batches_with_errors_in_multi_graph() -> Result<(), BoxError> {
    const REQUEST_COUNT: usize = 3;

    let requests_a = (0..REQUEST_COUNT).map(|index| {
        Request::fake_builder()
            .query(format!(
                "query op{index}{{ entryA(count: {REQUEST_COUNT}) {{ index }} }}"
            ))
            .build()
    });
    let requests_b = (0..REQUEST_COUNT).map(|index| {
        Request::fake_builder()
            .query(format!(
                "query op{index}{{ entryB(count: {REQUEST_COUNT}) {{ index }} }}"
            ))
            .build()
    });

    // Interleave requests so that we can verify that they get properly separated
    let requests: Vec<_> = requests_a.interleave(requests_b).collect();
    let responses = helper::run_test(
        CONFIG,
        &requests,
        Some(helper::fail_second_batch_request),
        Some(helper::fail_second_batch_request),
    )
    .await?;

    assert_yaml_snapshot!(responses, @r###"
    ---
    - data:
        entryA:
          index: 0
    - data:
        entryB:
          index: 0
    - errors:
        - message: expected error in A
    - errors:
        - message: expected error in B
    - data:
        entryA:
          index: 2
    - data:
        entryB:
          index: 2
    "###);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn it_handles_short_timeouts() -> Result<(), BoxError> {
    const REQUEST_COUNT: usize = 2;

    let requests_a = (0..REQUEST_COUNT).map(|index| {
        Request::fake_builder()
            .query(format!(
                "query op{index}{{ entryA(count: {REQUEST_COUNT}) {{ index }} }}"
            ))
            .build()
    });
    let requests_b = (0..REQUEST_COUNT).map(|index| {
        Request::fake_builder()
            .query(format!(
                "query op{index}{{ entryB(count: {REQUEST_COUNT}) {{ index }} }}"
            ))
            .build()
    });

    // Interleave requests so that we can verify that they get properly separated
    // Have the B subgraph timeout
    let requests: Vec<_> = requests_a.interleave(requests_b).collect();
    let responses = helper::run_test(
        SHORT_TIMEOUTS_CONFIG,
        &requests,
        Some(helper::expect_batch),
        Some(helper::never_respond),
    )
    .await?;

    assert_yaml_snapshot!(responses, @r###"
    ---
    - data:
        entryA:
          index: 0
    - errors:
        - message: "HTTP fetch failed from 'b': request timed out"
          path: []
          extensions:
            code: SUBREQUEST_HTTP_ERROR
            service: b
            reason: request timed out
    - data:
        entryA:
          index: 1
    - errors:
        - message: "HTTP fetch failed from 'b': request timed out"
          path: []
          extensions:
            code: SUBREQUEST_HTTP_ERROR
            service: b
            reason: request timed out
    "###);

    Ok(())
}

// This test makes two simultaneous requests to the router, with the first
// being never resolved. This is to make sure that the router doesn't hang while
// processing a separate batch request.
#[tokio::test(flavor = "multi_thread")]
async fn it_handles_indefinite_timeouts() -> Result<(), BoxError> {
    const REQUEST_COUNT: usize = 3;

    let requests_a: Vec<_> = (0..REQUEST_COUNT)
        .map(|index| {
            Request::fake_builder()
                .query(format!(
                    "query op{index}{{ entryA(count: {REQUEST_COUNT}) {{ index }} }}"
                ))
                .build()
        })
        .collect();
    let requests_b: Vec<_> = (0..REQUEST_COUNT)
        .map(|index| {
            Request::fake_builder()
                .query(format!(
                    "query op{index}{{ entryB(count: {REQUEST_COUNT}) {{ index }} }}"
                ))
                .build()
        })
        .collect();

    let responses_a = helper::run_test(
        SHORT_TIMEOUTS_CONFIG,
        &requests_a,
        Some(helper::expect_batch),
        None::<helper::Handler>,
    );
    let responses_b = helper::run_test(
        SHORT_TIMEOUTS_CONFIG,
        &requests_b,
        None::<helper::Handler>,
        Some(helper::never_respond),
    );

    // Run both requests simultaneously
    let (results_a, results_b) = futures::try_join!(responses_a, responses_b)?;

    // verify the output
    let responses = [results_a, results_b].concat();
    assert_yaml_snapshot!(responses, @r###"
    ---
    - data:
        entryA:
          index: 0
    - data:
        entryA:
          index: 1
    - data:
        entryA:
          index: 2
    - errors:
        - message: "HTTP fetch failed from 'b': request timed out"
          path: []
          extensions:
            code: SUBREQUEST_HTTP_ERROR
            service: b
            reason: request timed out
    - errors:
        - message: "HTTP fetch failed from 'b': request timed out"
          path: []
          extensions:
            code: SUBREQUEST_HTTP_ERROR
            service: b
            reason: request timed out
    - errors:
        - message: "HTTP fetch failed from 'b': request timed out"
          path: []
          extensions:
            code: SUBREQUEST_HTTP_ERROR
            service: b
            reason: request timed out
    "###);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn it_handles_cancelled_by_rhai() -> Result<(), BoxError> {
    const REQUEST_COUNT: usize = 2;
    const RHAI_CONFIG: &str = include_str!("../fixtures/batching/rhai_script.router.yaml");

    let requests_a = (0..REQUEST_COUNT).map(|index| {
        Request::fake_builder()
            .query(format!(
                "query op{index}{{ entryA(count: {REQUEST_COUNT}) {{ index }} }}"
            ))
            .build()
    });
    let requests_b = (0..REQUEST_COUNT).map(|index| {
        Request::fake_builder()
            .query(format!(
                "query op{index}{{ entryB(count: {REQUEST_COUNT}) {{ index }} }}"
            ))
            .build()
    });

    // Interleave requests so that we can verify that they get properly separated
    // Have the B subgraph get all of its requests cancelled by a rhai script
    let requests: Vec<_> = requests_a.interleave(requests_b).collect();
    let responses = helper::run_test(
        RHAI_CONFIG,
        &requests,
        Some(helper::expect_batch),
        None::<helper::Handler>,
    )
    .await?;

    // TODO: Fill this in once we know how this response should look
    assert_yaml_snapshot!(responses, @"");

    Ok(())
}

/// Utility methods for these tests
mod helper {
    use std::time::Duration;

    use apollo_router::graphql::Request;
    use apollo_router::graphql::Response;
    use tower::BoxError;
    use wiremock::matchers;
    use wiremock::MockServer;
    use wiremock::Respond;
    use wiremock::ResponseTemplate;

    use crate::integration::common::IntegrationTest;

    /// Helper type for specifying a valid handler
    pub type Handler = fn(&wiremock::Request) -> ResponseTemplate;

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
    pub async fn run_test<A: Respond + 'static, B: Respond + 'static>(
        config: &'static str,
        requests: &[Request],
        handler_a: Option<A>,
        handler_b: Option<B>,
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
        let mock_server_a = MockServer::start().await;
        let mock_server_b = MockServer::start().await;
        mock_server_a.register(make_handler!("/a", handler_a)).await;
        mock_server_b.register(make_handler!("/b", handler_b)).await;

        // Start up the router with the mocked subgraphs
        let mut router = IntegrationTest::builder()
            .config(config)
            .supergraph("tests/fixtures/batching/schema.graphql")
            .subgraph_override("a", format!("{}/a", mock_server_a.uri()))
            .subgraph_override("b", format!("{}/b", mock_server_b.uri()))
            .build()
            .await;

        router.start().await;
        router.assert_started().await;

        // Execute the request
        let request = serde_json::to_value(requests)?;
        let (_span, response) = router.execute_query(&request).await;

        serde_json::from_slice::<Vec<Response>>(&response.bytes().await?).map_err(BoxError::from)
    }

    /// Subgraph handler for receiving a batch of requests
    pub fn expect_batch(request: &wiremock::Request) -> ResponseTemplate {
        let requests: Vec<Request> = request.body_json().unwrap();

        // Extract info about this operation
        let (subgraph, count): (String, usize) = {
            let re = regex::Regex::new(r"entry([AB])\(count:([0-9]+)\)").unwrap();
            let captures = re.captures(requests[0].query.as_ref().unwrap()).unwrap();

            (captures[1].to_string(), captures[2].parse().unwrap())
        };

        // We should have gotten `count` elements
        assert_eq!(requests.len(), count);

        // Each element should have be for the specified subgraph and should have a field selection
        // of index.
        // Note: The router appends info to the query, so we append it at this check
        for (index, request) in requests.into_iter().enumerate() {
            assert_eq!(
                request.query,
                Some(format!(
                    "query op{index}__{}__0{{entry{}(count:{count}){{index}}}}",
                    subgraph.to_lowercase(),
                    subgraph
                ))
            );
        }

        ResponseTemplate::new(200).set_body_json(
            (0..count)
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

    /// Handler that always returns an error for the second batch field
    pub fn fail_second_batch_request(request: &wiremock::Request) -> ResponseTemplate {
        let requests: Vec<Request> = request.body_json().unwrap();

        // Extract info about this operation
        let (subgraph, count): (String, usize) = {
            let re = regex::Regex::new(r"entry([AB])\(count:([0-9]+)\)").unwrap();
            let captures = re.captures(requests[0].query.as_ref().unwrap()).unwrap();

            (captures[1].to_string(), captures[2].parse().unwrap())
        };

        // We should have gotten `count` elements
        assert_eq!(requests.len(), count);

        // Create the response with the second element as an error
        let responses = {
            let mut rs: Vec<_> = (0..count)
                .map(|index| {
                    serde_json::json!({
                        "data": {
                            format!("entry{subgraph}"): {
                                "index": index
                            }
                        }
                    })
                })
                .collect();

            rs[1] = serde_json::json!({ "errors": [{ "message": format!("expected error in {subgraph}") }] });
            rs
        };

        // Respond with an error on the second element but valid data for the rest
        ResponseTemplate::new(200).set_body_json(responses)
    }

    /// Subgraph handler that delays indefinitely
    ///
    /// Useful for testing timeouts at the batch level
    pub fn never_respond(request: &wiremock::Request) -> ResponseTemplate {
        let requests: Vec<Request> = request.body_json().unwrap();

        // Extract info about this operation
        let (_, count): (String, usize) = {
            let re = regex::Regex::new(r"entry([AB])\(count:([0-9]+)\)").unwrap();
            let captures = re.captures(requests[0].query.as_ref().unwrap()).unwrap();

            (captures[1].to_string(), captures[2].parse().unwrap())
        };

        // We should have gotten `count` elements
        assert_eq!(requests.len(), count);

        // Respond as normal but with a long delay
        ResponseTemplate::new(200).set_delay(Duration::from_secs(365 * 24 * 60 * 60))
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
