use apollo_router::graphql::Request;
use insta::assert_yaml_snapshot;
use itertools::Itertools;
use tower::BoxError;
use wiremock::ResponseTemplate;

use crate::integration::common::ValueExt as _;

const CONFIG: &str = include_str!("../fixtures/batching/all_enabled.router.yaml");
const SHORT_TIMEOUTS_CONFIG: &str = include_str!("../fixtures/batching/short_timeouts.router.yaml");

fn test_is_enabled() -> bool {
    std::env::var("TEST_APOLLO_KEY").is_ok() && std::env::var("TEST_APOLLO_GRAPH_REF").is_ok()
}

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

    if test_is_enabled() {
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
    }

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

    if test_is_enabled() {
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
    }

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

    if test_is_enabled() {
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
    }

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

    if test_is_enabled() {
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
    }

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

    if test_is_enabled() {
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
    }

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
    if test_is_enabled() {
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
    }

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
                "query op{index}_failMe{{ entryB(count: {REQUEST_COUNT}) {{ index }} }}"
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

    if test_is_enabled() {
        assert_yaml_snapshot!(responses, @r###"
    ---
    - data:
        entryA:
          index: 0
    - errors:
        - message: "rhai execution error: 'Runtime error: cancelled expected failure (line 5, position 13)\nin closure call'"
    - data:
        entryA:
          index: 1
    - errors:
        - message: "rhai execution error: 'Runtime error: cancelled expected failure (line 5, position 13)\nin closure call'"
    "###);
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn it_handles_single_request_cancelled_by_rhai() -> Result<(), BoxError> {
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
                "query {}{{ entryB(count: {REQUEST_COUNT}) {{ index }} }}",
                (index == 1)
                    .then_some("failMe".to_string())
                    .unwrap_or(format!("op{index}"))
            ))
            .build()
    });

    // Custom validation for subgraph B
    fn handle_b(request: &wiremock::Request) -> ResponseTemplate {
        let requests: Vec<Request> = request.body_json().unwrap();

        // We should have gotten all of the regular elements minus the second
        assert_eq!(requests.len(), REQUEST_COUNT - 1);

        // Each element should have be for the specified subgraph and should have a field selection
        // of index. The index should be 0..n without 1.
        // Note: The router appends info to the query, so we append it at this check
        for (request, index) in requests.into_iter().zip((0..).filter(|&i| i != 1)) {
            assert_eq!(
                request.query,
                Some(format!(
                    "query op{index}__b__0{{entryB(count:{REQUEST_COUNT}){{index}}}}",
                ))
            );
        }

        ResponseTemplate::new(200).set_body_json(
            (0..REQUEST_COUNT)
                .filter(|&i| i != 1)
                .map(|index| {
                    serde_json::json!({
                        "data": {
                            "entryB": {
                                "index": index
                            }
                        }
                    })
                })
                .collect::<Vec<_>>(),
        )
    }

    // Interleave requests so that we can verify that they get properly separated
    // Have the B subgraph get all of its requests cancelled by a rhai script
    let requests: Vec<_> = requests_a.interleave(requests_b).collect();
    let responses = helper::run_test(
        RHAI_CONFIG,
        &requests,
        Some(helper::expect_batch),
        Some(handle_b),
    )
    .await?;

    if test_is_enabled() {
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
    - errors:
        - message: "rhai execution error: 'Runtime error: cancelled expected failure (line 5, position 13)\nin closure call'"
    "###);
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn it_handles_cancelled_by_coprocessor() -> Result<(), BoxError> {
    const REQUEST_COUNT: usize = 2;
    const COPROCESSOR_CONFIG: &str = include_str!("../fixtures/batching/coprocessor.router.yaml");

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

    // Spin up a coprocessor for cancelling requests to A
    let coprocessor = wiremock::MockServer::builder().start().await;
    let subgraph_a_canceller = wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(|request: &wiremock::Request| {
            let info: serde_json::Value = request.body_json().unwrap();
            let subgraph = info
                .as_object()
                .unwrap()
                .get("serviceName")
                .unwrap()
                .as_string()
                .unwrap();

            // Pass through the request if the subgraph isn't 'A'
            let response = if subgraph != "a" {
                info
            } else {
                // Patch it otherwise to stop execution
                let mut res = info;
                let block = res.as_object_mut().unwrap();
                block.insert("control".to_string(), serde_json::json!({ "break": 403 }));
                block.insert(
                    "body".to_string(),
                    serde_json::json!({
                        "errors": [{
                            "message": "Subgraph A is not allowed",
                            "extensions": {
                                "code": "ERR_NOT_ALLOWED",
                            },
                        }],
                    }),
                );

                res
            };
            ResponseTemplate::new(200).set_body_json(response)
        })
        .named("coprocessor POST /");
    coprocessor.register(subgraph_a_canceller).await;

    // Make sure to patch the config with the coprocessor's port
    let config = COPROCESSOR_CONFIG.replace("REPLACEME", &coprocessor.address().port().to_string());

    // Interleave requests so that we can verify that they get properly separated
    // Have the A subgraph get all of its requests cancelled by a coprocessor
    let requests: Vec<_> = requests_a.interleave(requests_b).collect();
    let responses = helper::run_test(
        config.as_str(),
        &requests,
        None::<helper::Handler>,
        Some(helper::expect_batch),
    )
    .await?;

    if test_is_enabled() {
        assert_yaml_snapshot!(responses, @r###"
    ---
    - errors:
        - message: Subgraph A is not allowed
          extensions:
            code: ERR_NOT_ALLOWED
    - data:
        entryB:
          index: 0
    - errors:
        - message: Subgraph A is not allowed
          extensions:
            code: ERR_NOT_ALLOWED
    - data:
        entryB:
          index: 1
    "###);
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn it_handles_single_request_cancelled_by_coprocessor() -> Result<(), BoxError> {
    const REQUEST_COUNT: usize = 4;
    const COPROCESSOR_CONFIG: &str = include_str!("../fixtures/batching/coprocessor.router.yaml");

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

    // Spin up a coprocessor for cancelling requests to A
    let coprocessor = wiremock::MockServer::builder().start().await;
    let subgraph_a_canceller = wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(|request: &wiremock::Request| {
            let info: serde_json::Value = request.body_json().unwrap();
            let subgraph = info
                .as_object()
                .unwrap()
                .get("serviceName")
                .unwrap()
                .as_string()
                .unwrap();
            let query = info
                .as_object()
                .unwrap()
                .get("body")
                .unwrap()
                .as_object()
                .unwrap()
                .get("query")
                .unwrap()
                .as_string()
                .unwrap();

            // Cancel the request if we're in subgraph A, index 2
            let response = if subgraph == "a" && query.contains("op2") {
                // Patch it to stop execution
                let mut res = info;
                let block = res.as_object_mut().unwrap();
                block.insert("control".to_string(), serde_json::json!({ "break": 403 }));
                block.insert(
                    "body".to_string(),
                    serde_json::json!({
                        "errors": [{
                            "message": "Subgraph A index 2 is not allowed",
                            "extensions": {
                                "code": "ERR_NOT_ALLOWED",
                            },
                        }],
                    }),
                );

                res
            } else {
                info
            };
            ResponseTemplate::new(200).set_body_json(response)
        })
        .named("coprocessor POST /");
    coprocessor.register(subgraph_a_canceller).await;

    // We aren't expecting the whole batch anymore, so we need a handler here for it
    fn handle_a(request: &wiremock::Request) -> ResponseTemplate {
        let requests: Vec<Request> = request.body_json().unwrap();

        // We should have gotten all of the regular elements minus the third
        assert_eq!(requests.len(), REQUEST_COUNT - 1);

        // Each element should have be for the specified subgraph and should have a field selection
        // of index. The index should be 0..n without 2.
        // Note: The router appends info to the query, so we append it at this check
        for (request, index) in requests.into_iter().zip((0..).filter(|&i| i != 2)) {
            assert_eq!(
                request.query,
                Some(format!(
                    "query op{index}__a__0{{entryA(count:{REQUEST_COUNT}){{index}}}}",
                ))
            );
        }

        ResponseTemplate::new(200).set_body_json(
            (0..REQUEST_COUNT)
                .filter(|&i| i != 2)
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

    // Make sure to patch the config with the coprocessor's port
    let config = COPROCESSOR_CONFIG.replace("REPLACEME", &coprocessor.address().port().to_string());

    // Interleave requests so that we can verify that they get properly separated
    // Have the A subgraph get all of its requests cancelled by a coprocessor
    let requests: Vec<_> = requests_a.interleave(requests_b).collect();
    let responses = helper::run_test(
        config.as_str(),
        &requests,
        Some(handle_a),
        Some(helper::expect_batch),
    )
    .await?;

    if test_is_enabled() {
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
    - errors:
        - message: Subgraph A index 2 is not allowed
          extensions:
            code: ERR_NOT_ALLOWED
    - data:
        entryB:
          index: 2
    - data:
        entryA:
          index: 3
    - data:
        entryB:
          index: 3
    "###);
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn it_handles_single_invalid_graphql() -> Result<(), BoxError> {
    const REQUEST_COUNT: usize = 5;

    let mut requests: Vec<_> = (0..REQUEST_COUNT)
        .map(|index| {
            Request::fake_builder()
                .query(format!(
                    "query op{index}{{ entryA(count: {REQUEST_COUNT}) {{ index }} }}"
                ))
                .build()
        })
        .collect();

    // Mess up the 4th one
    requests[3].query = Some("query op3".into());

    // We aren't expecting the whole batch anymore, so we need a handler here for it
    fn handle_a(request: &wiremock::Request) -> ResponseTemplate {
        let requests: Vec<Request> = request.body_json().unwrap();

        // We should have gotten all of the regular elements minus the third
        assert_eq!(requests.len(), REQUEST_COUNT - 1);

        // Each element should have be for the specified subgraph and should have a field selection
        // of index. The index should be 0..n without 3.
        // Note: The router appends info to the query, so we append it at this check
        for (request, index) in requests.into_iter().zip((0..).filter(|&i| i != 3)) {
            assert_eq!(
                request.query,
                Some(format!(
                    "query op{index}__a__0{{entryA(count:{REQUEST_COUNT}){{index}}}}",
                ))
            );
        }

        ResponseTemplate::new(200).set_body_json(
            (0..REQUEST_COUNT)
                .filter(|&i| i != 3)
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

    let responses = helper::run_test(
        CONFIG,
        &requests[..],
        Some(handle_a),
        None::<helper::Handler>,
    )
    .await?;

    if test_is_enabled() {
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
        - errors:
            - message: "parsing error: syntax error: expected a Selection Set"
              locations:
                - line: 1
                  column: 10
              extensions:
                code: PARSING_ERROR
        - data:
            entryA:
              index: 4
        "###);
    }

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

    use super::test_is_enabled;
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
        config: &str,
        requests: &[Request],
        handler_a: Option<A>,
        handler_b: Option<B>,
    ) -> Result<Vec<Response>, BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if !test_is_enabled() {
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
