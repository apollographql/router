//! Subgraph traffic shaping as the subgraph stage places it.
//!
//! Each test builds the real subgraph stack with [`build_subgraph_services`]. A stub plugin
//! replaces the subgraph service with a [`tower_test::mock`] from its subgraph hook, so the mock
//! sits beneath traffic shaping and the tests see what the subgraph actually receives. Time is
//! paused, so timeouts and rate-limit intervals elapse deterministically.

use std::sync::Arc;
use std::time::Duration;

use http::HeaderValue;
use http::StatusCode;
use http::header::CONTENT_ENCODING;
use tower::BoxError;
use tower::Service;
use tower::ServiceExt;
use tower_test::mock::Mock;

use crate::Configuration;
use crate::pipeline::build_subgraph_services;
use crate::plugin::PluginInit;
use crate::plugin::PluginUnstable;
use crate::plugin::test::assert_no_mock_calls;
use crate::plugins::traffic_shaping::APOLLO_TRAFFIC_SHAPING;
use crate::services::Plugins;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;
use crate::services::SubgraphServices;
use crate::services::subgraph;

const SUBGRAPH: &str = "test";

type Handle = tower_test::mock::Handle<SubgraphRequest, SubgraphResponse>;

/// Replaces the subgraph service with `mock`.
struct StubSubgraph {
    mock: Mock<SubgraphRequest, SubgraphResponse>,
}

#[async_trait::async_trait]
impl PluginUnstable for StubSubgraph {
    type Config = ();

    async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        unreachable!("inserted into the plugin registry directly")
    }

    fn subgraph_service(
        &self,
        _subgraph_name: &str,
        _service: subgraph::BoxCloneService,
    ) -> subgraph::BoxCloneService {
        self.mock.clone().boxed_clone()
    }

    fn unstable_method(&self) {}
}

/// The subgraph services built with `traffic_shaping` config, and the handle of the mock that
/// stands in for the subgraph.
async fn subgraph_services(traffic_shaping: serde_json::Value) -> (SubgraphServices, Handle) {
    let mut plugins = Plugins::default();
    // The plugins the subgraph stage requires, in the order the router registers them.
    for (name, config) in [
        ("apollo.include_subgraph_errors", serde_json::json!({})),
        ("apollo.headers", serde_json::json!({})),
        (APOLLO_TRAFFIC_SHAPING, traffic_shaping),
    ] {
        let plugin = crate::plugin::plugins()
            .find(|factory| factory.name == name)
            .expect("plugin is registered")
            .create_instance_without_schema(&config)
            .await
            .expect("plugin builds");
        plugins.insert(name.to_string(), plugin);
    }
    let (mock, handle) = tower_test::mock::pair();
    plugins.insert("stub".to_string(), Box::new(StubSubgraph { mock }));

    let http_services = [(
        SUBGRAPH.to_string(),
        crate::services::http::test_http_client_service(SUBGRAPH),
    )]
    .into_iter()
    .collect();
    let services =
        build_subgraph_services(http_services, &Arc::new(plugins), &Configuration::default());
    (services, handle)
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
