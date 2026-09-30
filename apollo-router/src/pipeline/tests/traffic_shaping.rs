//! Tests of traffic shaping as the pipeline stage builders place it.
//!
//! Each test builds the real per-target stack with the pipeline's own builders. A stub plugin
//! stands in for every hook beneath traffic shaping, and time is paused, so timeouts and rate
//! limit intervals elapse deterministically.

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
use crate::plugin::PluginInit;
use crate::plugin::PluginUnstable;
use crate::plugins::traffic_shaping::APOLLO_TRAFFIC_SHAPING;
use crate::services::Plugins;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;
use crate::services::subgraph;

const SUBGRAPH: &str = "test";

type SubgraphFn = Arc<
    dyn Fn(SubgraphRequest) -> BoxFuture<'static, Result<SubgraphResponse, BoxError>> + Send + Sync,
>;

/// Stands in for everything beneath traffic shaping: its hook replaces the subgraph service
/// with the given function.
#[derive(Default)]
struct StubTargets {
    subgraph: Option<SubgraphFn>,
}

#[async_trait::async_trait]
impl PluginUnstable for StubTargets {
    type Config = ();

    async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        unreachable!("inserted into the plugin registry directly")
    }

    fn subgraph_service(
        &self,
        _subgraph_name: &str,
        service: subgraph::BoxCloneService,
    ) -> subgraph::BoxCloneService {
        match &self.subgraph {
            Some(stub) => {
                let stub = stub.clone();
                tower::service_fn(move |req| stub(req)).boxed_clone()
            }
            None => service,
        }
    }

    fn unstable_method(&self) {}
}

/// The plugin registry for a stack: the mandatory plugins the stage builders place, traffic
/// shaping built from `traffic_shaping`, and `stub` beneath them.
async fn plugins(traffic_shaping: serde_json::Value, stub: StubTargets) -> Arc<Plugins> {
    let mut plugins = Plugins::default();
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
    plugins.insert("stub".to_string(), Box::new(stub));
    Arc::new(plugins)
}

/// The placed service stack for [`SUBGRAPH`], with `stub` answering its requests.
async fn subgraph_stack(
    traffic_shaping: serde_json::Value,
    stub: impl Fn(SubgraphRequest) -> BoxFuture<'static, Result<SubgraphResponse, BoxError>>
    + Send
    + Sync
    + 'static,
) -> subgraph::BoxCloneService {
    let stub = StubTargets {
        subgraph: Some(Arc::new(stub)),
    };
    let http_services = [(
        SUBGRAPH.to_string(),
        crate::services::http::test_http_client_service(SUBGRAPH),
    )]
    .into_iter()
    .collect();
    crate::pipeline::build_subgraph_services(
        http_services,
        &plugins(traffic_shaping, stub).await,
        &Configuration::default(),
    )
    .get(SUBGRAPH)
    .expect("the subgraph has a service")
}

/// A subgraph that answers every request successfully after `delay`.
fn slow_subgraph(
    delay: Duration,
) -> impl Fn(SubgraphRequest) -> BoxFuture<'static, Result<SubgraphResponse, BoxError>> {
    move |req| {
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            Ok(SubgraphResponse::fake_builder()
                .context(req.context)
                .build())
        })
    }
}

fn error_code(response: &SubgraphResponse) -> Option<String> {
    response.response.body().errors.first()?.extension_code()
}

#[tokio::test(start_paused = true)]
async fn subgraph_timeout_is_answered_with_gateway_timeout() {
    let service = subgraph_stack(
        serde_json::json!({ "subgraphs": { SUBGRAPH: { "timeout": "100ms" } } }),
        slow_subgraph(Duration::from_secs(1)),
    )
    .await;

    let response = service
        .oneshot(SubgraphRequest::fake_builder().build())
        .await
        .expect("admission renders the timeout as a response");

    assert_eq!(response.response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(error_code(&response).as_deref(), Some("GATEWAY_TIMEOUT"));
}

#[tokio::test(start_paused = true)]
async fn subgraph_without_shaping_config_is_not_shaped() {
    // With no `all` or subgraph block, not even the 30 second default timeout applies.
    let service = subgraph_stack(
        serde_json::json!({}),
        slow_subgraph(Duration::from_secs(60)),
    )
    .await;

    let response = service
        .oneshot(SubgraphRequest::fake_builder().build())
        .await
        .unwrap();

    assert_eq!(response.response.status(), StatusCode::OK);
    assert!(response.response.body().errors.is_empty());
}

#[tokio::test(start_paused = true)]
async fn subgraph_rate_limit_is_answered_with_service_unavailable() {
    let mut service = subgraph_stack(
        serde_json::json!({ "all": {
            "global_rate_limit": { "capacity": 1, "interval": "100ms" }
        } }),
        slow_subgraph(Duration::ZERO),
    )
    .await;

    let first = service
        .ready()
        .await
        .unwrap()
        .call(SubgraphRequest::fake_builder().build())
        .await
        .unwrap();
    assert_eq!(first.response.status(), StatusCode::OK);

    let limited = service
        .ready()
        .await
        .unwrap()
        .call(SubgraphRequest::fake_builder().build())
        .await
        .unwrap();
    assert_eq!(limited.response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        error_code(&limited).as_deref(),
        Some("REQUEST_RATE_LIMITED")
    );

    tokio::time::advance(Duration::from_millis(100)).await;

    let after_interval = service
        .ready()
        .await
        .unwrap()
        .call(SubgraphRequest::fake_builder().build())
        .await
        .unwrap();
    assert_eq!(after_interval.response.status(), StatusCode::OK);
}

/// Sends two identical requests concurrently and returns how many reached the subgraph.
async fn concurrent_identical_requests(deduplicate_query: bool) -> usize {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let slow = slow_subgraph(Duration::from_secs(1));
    let service = subgraph_stack(
        serde_json::json!({ "subgraphs": { SUBGRAPH: { "deduplicate_query": deduplicate_query } } }),
        move |req| {
            counted.fetch_add(1, Ordering::SeqCst);
            slow(req)
        },
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
async fn subgraph_deduplication_shares_one_in_flight_request() {
    assert_eq!(concurrent_identical_requests(true).await, 1);
    assert_eq!(concurrent_identical_requests(false).await, 2);
}

#[tokio::test(start_paused = true)]
async fn subgraph_compression_sets_content_encoding() {
    let service = subgraph_stack(
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

    let response = service
        .oneshot(SubgraphRequest::fake_builder().build())
        .await
        .unwrap();
    assert_eq!(response.response.status(), StatusCode::OK);
}
