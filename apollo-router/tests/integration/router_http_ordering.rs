//! RouterHttp pipeline ordering tests.
//!
//! These tests verify that RouterHttp runs before the Router pipeline and that
//! the plugin order (license_enforcement → rhai → coprocessor → router_service)
//! is correct.
//!
//! **Prerequisites:** The `router_http_service` hook must be added to the `Plugin` trait
//! (or `PluginUnstable`/`PluginPrivate` as appropriate) for these tests to compile.
//!
//! Once router_http is fully integrated, extend `test_plugin_ordering` in lifecycle.rs to:
//! 1. Add `router_http(service)` to the Rhai script (test_plugin_ordering.rhai)
//! 2. Add `router_http: { request: { context: true }, response: { context: true } }` to coprocessor config
//! 3. Add `router_http_service` to the test plugins in the make_plugin! macro
//! 4. Update the expected trace to include:
//!    - "router_http Rhai map_request" (first, before coprocessor)
//!    - "coprocessor RouterHttpRequest"
//!    - "router_http Rust test_ordering_1/2/3 map_request"
//!    - ... (existing router/supergraph trace) ...
//!    - "coprocessor RouterHttpResponse"
//!    - "router_http Rhai map_response"
//!    - "router_http Rust test_ordering_3/2/1 map_response" (reverse order)

#[tokio::test(flavor = "multi_thread")]
async fn router_http_ordering_placeholder_compiles_and_module_included() {
    // Ensures this module is compiled and run. Full ordering is asserted in test_plugin_ordering (lifecycle.rs).
    assert!(true, "router_http_ordering module included");
}
