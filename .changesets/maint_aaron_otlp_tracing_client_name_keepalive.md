### Fix Linux flake in otlp::tracing::test_plugin_overridden_client_name_is_included_in_telemetry

`integration::telemetry::otlp::tracing::test_plugin_overridden_client_name_is_included_in_telemetry` flaked on CircleCI's Linux executor with `unable to send successful request to router, error sending request for url (http://127.0.0.1:<port>/)` from `IntegrationTest::execute_query`. The first two iterations of the test loop completed (visible as two successful trace verifications in the captured stdout); the third iteration's outbound HTTP request to the spawned router never reached the router. See CircleCI job 378759.

**Root cause**

The test runs four sequential `validate_otlp_trace` iterations against the same long-lived router. Between iterations, `Verifier::validate_trace` polls `find_valid_trace` at 50 ms intervals for up to 10 s while the harness's default `reqwest::Client` keeps an idle inbound TCP connection pooled to the router. Under CI load the pooled HTTP/1 keep-alive connection can be reset (by the router-side connection task or by the host network stack) before the next iteration reuses it; `reqwest` surfaces the reset as a connection-level `error sending request`. The router itself is still running fine — the failure is purely on the stale pooled connection.

**Fix**

Wire a `reqwest::Client::builder().pool_max_idle_per_host(0)` client into the test via `IntegrationTest::builder().reqwest_client(...)`. Each iteration now opens its own TCP connection to the router, eliminating the stale-pooled-connection race. This matches the established `no_keepalive_reqwest_client` pattern already used in `tests/integration/coprocessor.rs`, `tests/integration/subgraph_response.rs`, and `tests/integration/file_upload.rs` for the same class of flake (T17 sibling). No deadline widening; no test-level retries.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9497
