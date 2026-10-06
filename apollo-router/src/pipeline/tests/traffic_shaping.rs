//! Subgraph and connector source traffic shaping as their stages place it.
//!
//! Each test builds the real stack with the stage builders, through [`stage_stack`], which puts a
//! [`tower_test::mock`] beneath traffic shaping, so the tests see what the target actually
//! receives. Time is paused, so timeouts and rate-limit intervals elapse deterministically.

use std::time::Duration;

use apollo_federation::connectors::runtime::errors::Error;
use http::HeaderValue;
use http::StatusCode;
use http::header::CONTENT_ENCODING;
use tower::Service;
use tower::ServiceExt;

use super::stage_stack;
use super::stage_stack::Handle;
use super::stage_stack::SourceHandle;
use crate::plugin::test::assert_no_mock_calls;
use crate::plugins::traffic_shaping::APOLLO_TRAFFIC_SHAPING;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;
use crate::services::SubgraphServices;
use crate::services::connector::request_service;
use crate::services::http::HttpRequest;
use crate::services::http::HttpResponse;
use crate::services::router;
use crate::services::subgraph;

const SUBGRAPH: &str = "test";
/// A connector source key, `<subgraph name>.<source name>`, matching
/// [`request_service::Request::test_new`].
const SOURCE: &str = "test_subgraph.test_sourcename";

/// The subgraph services built with `traffic_shaping` config, and the handle of the mock that
/// stands in for the subgraph.
async fn subgraph_services(traffic_shaping: serde_json::Value) -> (SubgraphServices, Handle) {
    stage_stack::subgraph_services(SUBGRAPH, &[(APOLLO_TRAFFIC_SHAPING, traffic_shaping)]).await
}

/// Like [`subgraph_services`], for one clone of the test subgraph's service.
async fn subgraph_service(
    traffic_shaping: serde_json::Value,
) -> (subgraph::BoxCloneService, Handle) {
    let (services, handle) = subgraph_services(traffic_shaping).await;
    (
        services.get(SUBGRAPH).expect("the subgraph has a service"),
        handle,
    )
}

/// Answers the next request the subgraph receives successfully, and returns that request.
async fn answer_next(handle: &mut Handle) -> SubgraphRequest {
    let (request, response) = handle
        .next_request()
        .await
        .expect("the subgraph receives a request");
    response.send_response(
        SubgraphResponse::fake_builder()
            .context(request.context.clone())
            .build(),
    );
    request
}

fn rate_limit_of_one_per_100ms() -> serde_json::Value {
    serde_json::json!({ "all": {
        "global_rate_limit": { "capacity": 1, "interval": "100ms" }
    } })
}

async fn send(service: &mut subgraph::BoxCloneService) -> SubgraphResponse {
    service
        .ready()
        .await
        .unwrap()
        .call(SubgraphRequest::fake_builder().build())
        .await
        .expect("traffic shaping answers with a response, not an error")
}

fn error_code(response: &SubgraphResponse) -> Option<String> {
    response.response.body().errors.first()?.extension_code()
}

#[tokio::test(start_paused = true)]
async fn timeout_is_answered_with_gateway_timeout() {
    let (mut service, mut handle) =
        subgraph_service(serde_json::json!({ "subgraphs": { SUBGRAPH: { "timeout": "100ms" } } }))
            .await;

    // The subgraph never answers.
    let response = send(&mut service).await;
    assert_eq!(response.response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(error_code(&response).as_deref(), Some("GATEWAY_TIMEOUT"));
    assert!(handle.next_request().await.is_some());
}

#[tokio::test(start_paused = true)]
async fn subgraph_without_shaping_config_is_not_shaped() {
    let (mut service, mut handle) = subgraph_service(serde_json::json!({})).await;

    // With no `all` or subgraph block, not even the 30 second default timeout applies.
    let (response, ()) = tokio::join!(send(&mut service), async {
        let (request, response) = handle.next_request().await.unwrap();
        tokio::time::sleep(Duration::from_secs(60)).await;
        response.send_response(
            SubgraphResponse::fake_builder()
                .context(request.context)
                .build(),
        );
    });
    assert_eq!(response.response.status(), StatusCode::OK);
    assert!(response.response.body().errors.is_empty());
}

#[tokio::test(start_paused = true)]
async fn rate_limited_request_never_reaches_the_subgraph() {
    let (mut service, mut handle) = subgraph_service(rate_limit_of_one_per_100ms()).await;

    let (first, _) = tokio::join!(send(&mut service), answer_next(&mut handle));
    assert_eq!(first.response.status(), StatusCode::OK);

    let limited = send(&mut service).await;
    assert_eq!(limited.response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        error_code(&limited).as_deref(),
        Some("REQUEST_RATE_LIMITED")
    );

    tokio::time::advance(Duration::from_millis(100)).await;

    let (after_interval, _) = tokio::join!(send(&mut service), answer_next(&mut handle));
    assert_eq!(after_interval.response.status(), StatusCode::OK);
    // Only the first and last of the three requests reached the subgraph.
    assert_no_mock_calls(handle).await;
}

#[tokio::test(start_paused = true)]
async fn clones_of_the_service_share_one_rate_limit() {
    let (services, mut handle) = subgraph_services(rate_limit_of_one_per_100ms()).await;
    let mut first = services.get(SUBGRAPH).unwrap();
    let mut second = services.get(SUBGRAPH).unwrap();

    let (ok, _) = tokio::join!(send(&mut first), answer_next(&mut handle));
    assert_eq!(ok.response.status(), StatusCode::OK);
    assert_eq!(
        send(&mut second).await.response.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_no_mock_calls(handle).await;
}

/// Sends two identical requests concurrently, and checks that `expected` of them reach the
/// subgraph.
async fn identical_requests_reach_the_subgraph(deduplicate_query: bool, expected: usize) {
    let (service, mut handle) = subgraph_service(
        serde_json::json!({ "subgraphs": { SUBGRAPH: { "deduplicate_query": deduplicate_query } } }),
    )
    .await;

    let answer = async {
        let mut received = Vec::new();
        for _ in 0..expected {
            received.push(handle.next_request().await.unwrap());
        }
        // Paused time only advances once every task is idle, so the other request has reached
        // deduplication before the subgraph answers.
        tokio::time::sleep(Duration::from_millis(1)).await;
        for (request, response) in received {
            response.send_response(
                SubgraphResponse::fake_builder()
                    .context(request.context)
                    .build(),
            );
        }
    };
    let (first, second, ()) = tokio::join!(
        service
            .clone()
            .oneshot(SubgraphRequest::fake_builder().build()),
        service.oneshot(SubgraphRequest::fake_builder().build()),
        answer,
    );
    assert_eq!(first.unwrap().response.status(), StatusCode::OK);
    assert_eq!(second.unwrap().response.status(), StatusCode::OK);
    assert_no_mock_calls(handle).await;
}

#[tokio::test(start_paused = true)]
async fn deduplication_shares_one_in_flight_request() {
    identical_requests_reach_the_subgraph(true, 1).await;
    identical_requests_reach_the_subgraph(false, 2).await;
}

/// The timeout sits below deduplication, so a request joined to one already in flight is answered
/// when that one times out, and the subgraph receives one request.
#[tokio::test(start_paused = true)]
async fn deduplicated_requests_share_one_timeout() {
    let (service, mut handle) = subgraph_service(serde_json::json!({ "subgraphs": { SUBGRAPH: {
        "deduplicate_query": true,
        "timeout": "100ms",
    } } }))
    .await;

    // The subgraph never answers.
    let (first, second, received) = tokio::join!(
        service
            .clone()
            .oneshot(SubgraphRequest::fake_builder().build()),
        service.oneshot(SubgraphRequest::fake_builder().build()),
        handle.next_request(),
    );
    assert!(received.is_some());
    for response in [first.unwrap(), second.unwrap()] {
        assert_eq!(response.response.status(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(error_code(&response).as_deref(), Some("GATEWAY_TIMEOUT"));
    }
    drop(received);
    assert_no_mock_calls(handle).await;
}

#[tokio::test(start_paused = true)]
async fn compression_sets_content_encoding() {
    let (mut service, mut handle) = subgraph_service(
        serde_json::json!({ "subgraphs": { SUBGRAPH: { "compression": "gzip" } } }),
    )
    .await;

    let (response, request) = tokio::join!(send(&mut service), answer_next(&mut handle));
    assert_eq!(response.response.status(), StatusCode::OK);
    assert_eq!(
        request.subgraph_request.headers().get(CONTENT_ENCODING),
        Some(&HeaderValue::from_static("gzip"))
    );
}

/// A function that hands out clones of [`SOURCE`]'s request service, built with
/// `traffic_shaping` config, and the handle of the mock that stands in for the source.
async fn source_services(
    traffic_shaping: serde_json::Value,
) -> (impl Fn() -> request_service::BoxCloneService, SourceHandle) {
    stage_stack::source_services(SOURCE, &[(APOLLO_TRAFFIC_SHAPING, traffic_shaping)]).await
}

async fn answer_next_source(handle: &mut SourceHandle) -> HttpRequest {
    let (request, response) = handle
        .next_request()
        .await
        .expect("the connector source receives a request");
    response.send_response(source_response(&request));
    request
}

fn source_response(request: &HttpRequest) -> HttpResponse {
    HttpResponse {
        http_response: http::Response::new(router::body::empty()),
        context: request.context.clone(),
    }
}

fn source_rate_limit_of_one_per_100ms() -> serde_json::Value {
    serde_json::json!({ "connector": { "all": {
        "global_rate_limit": { "capacity": 1, "interval": "100ms" }
    } } })
}

async fn send_to_source(service: &mut request_service::BoxCloneService) -> Result<(), Error> {
    let response = service
        .ready()
        .await
        .unwrap()
        .call(request_service::Request::test_new())
        .await
        .expect("traffic shaping answers with a response, not an error");
    match response.transport_outcome {
        request_service::TransportOutcome::Error(error) => Err(error),
        _ => Ok(()),
    }
}

#[tokio::test(start_paused = true)]
async fn source_timeout_is_answered_with_gateway_timeout() {
    let (service, mut handle) = source_services(
        serde_json::json!({ "connector": { "sources": { SOURCE: { "timeout": "100ms" } } } }),
    )
    .await;

    // The source never answers.
    assert!(matches!(
        send_to_source(&mut service()).await,
        Err(Error::GatewayTimeout)
    ));
    assert!(handle.next_request().await.is_some());
}

#[tokio::test(start_paused = true)]
async fn source_without_shaping_config_is_not_shaped() {
    let (service, mut handle) = source_services(serde_json::json!({})).await;
    let mut service = service();

    // With no `connector.all` or source block, not even the 30 second default timeout applies.
    let (result, ()) = tokio::join!(send_to_source(&mut service), async {
        let (request, response) = handle.next_request().await.unwrap();
        tokio::time::sleep(Duration::from_secs(60)).await;
        response.send_response(source_response(&request));
    });
    assert!(result.is_ok());
}

#[tokio::test(start_paused = true)]
async fn rate_limited_request_never_reaches_the_source() {
    let (service, mut handle) = source_services(source_rate_limit_of_one_per_100ms()).await;
    let mut service = service();

    let (first, _) = tokio::join!(
        send_to_source(&mut service),
        answer_next_source(&mut handle)
    );
    assert!(first.is_ok());

    assert!(matches!(
        send_to_source(&mut service).await,
        Err(Error::RateLimited)
    ));

    tokio::time::advance(Duration::from_millis(100)).await;

    let (after_interval, _) = tokio::join!(
        send_to_source(&mut service),
        answer_next_source(&mut handle)
    );
    assert!(after_interval.is_ok());
    // Only the first and last of the three requests reached the source.
    assert_no_mock_calls(handle).await;
}

#[tokio::test(start_paused = true)]
async fn clones_of_the_source_service_share_one_rate_limit() {
    let (service, mut handle) = source_services(source_rate_limit_of_one_per_100ms()).await;
    let (mut first, mut second) = (service(), service());

    let (ok, _) = tokio::join!(send_to_source(&mut first), answer_next_source(&mut handle));
    assert!(ok.is_ok());
    assert!(matches!(
        send_to_source(&mut second).await,
        Err(Error::RateLimited)
    ));
    assert_no_mock_calls(handle).await;
}

#[tokio::test(start_paused = true)]
async fn source_compression_sets_content_encoding() {
    let (service, mut handle) = source_services(
        serde_json::json!({ "connector": { "sources": { SOURCE: { "compression": "gzip" } } } }),
    )
    .await;
    let mut service = service();

    let (result, request) = tokio::join!(
        send_to_source(&mut service),
        answer_next_source(&mut handle)
    );
    assert!(result.is_ok());
    assert_eq!(
        request.http_request.headers().get(CONTENT_ENCODING),
        Some(&HeaderValue::from_static("gzip"))
    );
}
