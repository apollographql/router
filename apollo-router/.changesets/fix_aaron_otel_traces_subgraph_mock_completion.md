### Fix `apollo_otel_traces` live-network flake for 7 remaining tests (ROUTER-1878)

7 tests in `tests/apollo_otel_traces.rs` still called `get_trace_report`, which routes
through `with_subgraph_network_requests()` to `https://*.demo.starstuff.dev/`. When
those hosts reset the connection (Windows os error 10054, Linux ECONNRESET), the subgraph
fetch fails and `apollo_private.ftv1` is stripped from the OTel span, breaking the
snapshot assertion.

Migrates all 7 remaining callers (`non_defer`, `test_condition_if`, `test_condition_else`,
`test_trace_id`, `test_client_name`, `test_client_version`, `test_send_header`) to
`get_trace_report_with_subgraph_mock`, which routes through the localhost wiremock
introduced in ROUTER-1834. No snapshot re-bless needed: `assert_report!` redacts FTV1
to `"[redacted]"` regardless of source.
