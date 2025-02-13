use std::sync::atomic::Ordering;

use tower::Service as _;
use tower::ServiceExt as _;

// This test is separated because it needs to run in a dedicated process.
// nextest does this by default, but using a separate [[test]] also makes it work with `cargo test`.

#[tokio::test]
async fn test_compute_backpressure_response() {
    let mut harness = apollo_router::TestHarness::builder()
        .build_router()
        .await
        .unwrap();
    macro_rules! call {
        ($query: expr) => {{
            let request = apollo_router::services::supergraph::Request::canned_builder()
                .query($query)
                .build()
                .unwrap()
                .try_into()
                .unwrap();
            let mut response = harness.ready().await.unwrap().call(request).await.unwrap();
            let bytes = response.next_response().await.unwrap().unwrap();
            let graphql_response: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            serde_json::json!({
                "status": response.response.status().as_u16(),
                "body": graphql_response
            })
        }};
    }

    insta::assert_yaml_snapshot!(call!("{__typename} # 1"), @r###"
        status: 200
        body:
          data:
            __typename: Query
    "###);

    apollo_router::_private::compute_job_queued_count().fetch_add(1_000_000, Ordering::Relaxed);

    // Slightly different query so parsing isn’t cached and "compute" is needed.
    // When the queue is full we quickly return an error
    let start = std::time::Instant::now();
    let response = call!("{__typename} # 2");
    let latency = start.elapsed();
    assert!(
        latency < std::time::Duration::from_millis(100),
        "latency = {latency:?}, status = {:?}",
        &response["status"]
    );
    insta::assert_yaml_snapshot!(response, @r###"
        status: 503
        body:
          errors:
            - message: Your request has been concurrency limited during query processing
              extensions:
                code: REQUEST_CONCURRENCY_LIMITED
    "###);

    apollo_router::_private::compute_job_queued_count().fetch_sub(1_000_000, Ordering::Relaxed);

    // Router gracefully recovers
    insta::assert_yaml_snapshot!(call!("{__typename} # 3"), @r###"
        status: 200
        body:
          data:
            __typename: Query
    "###);
}
