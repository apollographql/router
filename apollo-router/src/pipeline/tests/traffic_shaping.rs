//! Subgraph traffic shaping as the subgraph stage places it.
//!
//! Each test builds the real subgraph stack with [`build_subgraph_services`]. A stub plugin
//! answers every request from its subgraph hook, so it sits beneath traffic shaping, and time is
//! paused, so timeouts and rate-limit intervals elapse deterministically.

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use futures::future::BoxFuture;
use http::HeaderValue;
use http::StatusCode;
use http::header::CONTENT_ENCODING;
use tower::BoxError;
use tower::Service;
use tower::ServiceExt;

use crate::Configuration;
use crate::pipeline::build_subgraph_services;
use crate::plugin::PluginInit;
use crate::plugin::PluginUnstable;
use crate::plugins::traffic_shaping::APOLLO_TRAFFIC_SHAPING;
use crate::services::Plugins;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;
use crate::services::SubgraphServices;
use crate::services::subgraph;

const SUBGRAPH: &str = "test";

type Responder = Arc<
    dyn Fn(SubgraphRequest) -> BoxFuture<'static, Result<SubgraphResponse, BoxError>> + Send + Sync,
>;

/// Replaces the subgraph service with `respond`, and counts the requests that reach it.
struct StubSubgraph {
    respond: Responder,
    calls: Arc<AtomicUsize>,
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
        let respond = self.respond.clone();
        let calls = self.calls.clone();
        tower::service_fn(move |req| {
            calls.fetch_add(1, Ordering::SeqCst);
            respond(req)
        })
        .boxed_clone()
    }

    fn unstable_method(&self) {}
}

/// The subgraph services built with `traffic_shaping` config, answered by `respond`, and the
/// count of requests that reached `respond`.
async fn subgraph_services(
    traffic_shaping: serde_json::Value,
    respond: impl Fn(SubgraphRequest) -> BoxFuture<'static, Result<SubgraphResponse, BoxError>>
    + Send
    + Sync
    + 'static,
) -> (SubgraphServices, Arc<AtomicUsize>) {
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
    let calls = Arc::new(AtomicUsize::new(0));
    let stub = StubSubgraph {
        respond: Arc::new(respond),
        calls: calls.clone(),
    };
    plugins.insert("stub".to_string(), Box::new(stub));

    let http_services = [(
        SUBGRAPH.to_string(),
        crate::services::http::test_http_client_service(SUBGRAPH),
    )]
    .into_iter()
    .collect();
    let services =
        build_subgraph_services(http_services, &Arc::new(plugins), &Configuration::default());
    (services, calls)
}

/// Like [`subgraph_services`], for one clone of the test subgraph's service.
async fn subgraph_service(
    traffic_shaping: serde_json::Value,
    respond: impl Fn(SubgraphRequest) -> BoxFuture<'static, Result<SubgraphResponse, BoxError>>
    + Send
    + Sync
    + 'static,
) -> (subgraph::BoxCloneService, Arc<AtomicUsize>) {
    let (services, calls) = subgraph_services(traffic_shaping, respond).await;
    (
        services.get(SUBGRAPH).expect("the subgraph has a service"),
        calls,
    )
}

/// Answers every request successfully after `delay`.
fn respond_after(
    delay: Duration,
) -> impl Fn(SubgraphRequest) -> BoxFuture<'static, Result<SubgraphResponse, BoxError>> + Clone {
    move |req| {
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            Ok(SubgraphResponse::fake_builder()
                .context(req.context)
                .build())
        })
    }
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
    let (mut service, _) = subgraph_service(
        serde_json::json!({ "subgraphs": { SUBGRAPH: { "timeout": "100ms" } } }),
        respond_after(Duration::from_secs(1)),
    )
    .await;

    let response = send(&mut service).await;
    assert_eq!(response.response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(error_code(&response).as_deref(), Some("GATEWAY_TIMEOUT"));
}

#[tokio::test(start_paused = true)]
async fn subgraph_without_shaping_config_is_not_shaped() {
    // With no `all` or subgraph block, not even the 30 second default timeout applies.
    let (mut service, _) = subgraph_service(
        serde_json::json!({}),
        respond_after(Duration::from_secs(60)),
    )
    .await;

    let response = send(&mut service).await;
    assert_eq!(response.response.status(), StatusCode::OK);
    assert!(response.response.body().errors.is_empty());
}

#[tokio::test(start_paused = true)]
async fn rate_limited_request_never_reaches_the_subgraph() {
    let (mut service, calls) =
        subgraph_service(rate_limit_of_one_per_100ms(), respond_after(Duration::ZERO)).await;

    assert_eq!(send(&mut service).await.response.status(), StatusCode::OK);

    let limited = send(&mut service).await;
    assert_eq!(limited.response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        error_code(&limited).as_deref(),
        Some("REQUEST_RATE_LIMITED")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    tokio::time::advance(Duration::from_millis(100)).await;

    assert_eq!(send(&mut service).await.response.status(), StatusCode::OK);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn clones_of_the_service_share_one_rate_limit() {
    let (services, calls) =
        subgraph_services(rate_limit_of_one_per_100ms(), respond_after(Duration::ZERO)).await;
    let mut first = services.get(SUBGRAPH).unwrap();
    let mut second = services.get(SUBGRAPH).unwrap();

    assert_eq!(send(&mut first).await.response.status(), StatusCode::OK);
    assert_eq!(
        send(&mut second).await.response.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Sends two identical requests concurrently and returns how many reached the subgraph.
async fn concurrent_identical_requests(deduplicate_query: bool) -> usize {
    let (service, calls) = subgraph_service(
        serde_json::json!({ "subgraphs": { SUBGRAPH: { "deduplicate_query": deduplicate_query } } }),
        respond_after(Duration::from_secs(1)),
    )
    .await;

    let (first, second) = tokio::join!(
        service
            .clone()
            .oneshot(SubgraphRequest::fake_builder().build()),
        service.oneshot(SubgraphRequest::fake_builder().build()),
    );
    assert_eq!(first.unwrap().response.status(), StatusCode::OK);
    assert_eq!(second.unwrap().response.status(), StatusCode::OK);

    calls.load(Ordering::SeqCst)
}

#[tokio::test(start_paused = true)]
async fn deduplication_shares_one_in_flight_request() {
    assert_eq!(concurrent_identical_requests(true).await, 1);
    assert_eq!(concurrent_identical_requests(false).await, 2);
}

#[tokio::test(start_paused = true)]
async fn compression_sets_content_encoding() {
    let (mut service, _) = subgraph_service(
        serde_json::json!({ "subgraphs": { SUBGRAPH: { "compression": "gzip" } } }),
        |req: SubgraphRequest| {
            let encoding = req
                .subgraph_request
                .headers()
                .get(CONTENT_ENCODING)
                .cloned();
            Box::pin(async move {
                assert_eq!(encoding, Some(HeaderValue::from_static("gzip")));
                Ok(SubgraphResponse::fake_builder()
                    .context(req.context)
                    .build())
            })
        },
    )
    .await;

    assert_eq!(send(&mut service).await.response.status(), StatusCode::OK);
}
