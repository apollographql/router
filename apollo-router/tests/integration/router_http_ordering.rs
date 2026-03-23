//! RouterHttp pipeline ordering tests.
//!
//! These tests verify that the **RouterHttp** pipeline runs first (top-level hook), then the
//! **Router** pipeline. Plugin add order is the same for both; see `router_factory::create_plugins`
//! and [request lifecycle](https://www.apollographql.com/docs/graphos/routing/request-lifecycle).
//!
//! **Prerequisites:** The `router_http_service` hook must be added to the `Plugin` trait
//! (or `PluginUnstable`/`PluginPrivate` as appropriate) for these tests to compile.
//!
//! Full ordering with Rhai and coprocessor is asserted in `test_plugin_ordering` (lifecycle.rs).

use apollo_router::TestHarness;
use apollo_router::services::router;
use apollo_router::services::supergraph;
use serde_json::json;
use tower::Service;
use tower::ServiceBuilder;
use tower::ServiceExt;

const ROUTER_HTTP_ORDER_CONTEXT_KEY: &str = "router_http_order";

/// Minimal assertion that RouterHttp runs before the Router pipeline.
#[tokio::test(flavor = "multi_thread")]
async fn router_http_runs_before_router_pipeline() {
    let mut service = TestHarness::builder()
        .router_http_hook(|service| {
            ServiceBuilder::new()
                .map_request(|request: router::Request| {
                    request
                        .context
                        .upsert(ROUTER_HTTP_ORDER_CONTEXT_KEY, |mut order: Vec<String>| {
                            order.push("router_http".to_string());
                            order
                        })
                        .unwrap();
                    request
                })
                .service(service)
                .boxed()
        })
        .router_hook(|service| {
            ServiceBuilder::new()
                .map_request(|request: router::Request| {
                    request
                        .context
                        .upsert(ROUTER_HTTP_ORDER_CONTEXT_KEY, |mut order: Vec<String>| {
                            order.push("router_service".to_string());
                            order
                        })
                        .unwrap();
                    request
                })
                .service(service)
                .boxed()
        })
        .configuration_json(json!({}))
        .unwrap()
        .build_router()
        .await
        .unwrap();

    let request = supergraph::Request::canned_builder().build().unwrap();
    let mut response = service
        .ready()
        .await
        .unwrap()
        .call(request.try_into().unwrap())
        .await
        .unwrap();

    let _ = response.next_response().await.unwrap().unwrap();
    let order: Vec<String> = response
        .context
        .get(ROUTER_HTTP_ORDER_CONTEXT_KEY)
        .unwrap()
        .unwrap_or_default();

    assert_eq!(
        order,
        ["router_http", "router_service"],
        "RouterHttp must run before Router pipeline"
    );
}

/// Plugin that only implements router_service (not router_http_service) must not break the pipeline.
#[tokio::test(flavor = "multi_thread")]
async fn default_no_op_router_http_service_does_not_break_pipeline() {
    let mut service = TestHarness::builder()
        .router_hook(|service| service)
        .configuration_json(json!({}))
        .unwrap()
        .build_router()
        .await
        .unwrap();

    let request = supergraph::Request::canned_builder().build().unwrap();
    let mut response = service
        .ready()
        .await
        .unwrap()
        .call(request.try_into().unwrap())
        .await
        .unwrap();

    let chunk = response.next_response().await.unwrap();
    assert!(
        chunk.is_ok(),
        "request must succeed when plugin omits router_http_service"
    );
}
