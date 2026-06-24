### Fix Linux flakes in subscriptions::callback startup race pair

Two sibling tests in `integration::subscriptions::callback` flaked on Linux CI on `test-amd_linux_test` with the same `assert_started`-vs-accept-loop race:

- `test_subscription_callback_pure_error_payload` (CircleCI build 378842, 2026-05-26) panicked at `tests/integration/../common.rs:1412:25` with `unable to send successful request to router, error sending request for url (...)`. Test elapsed 1.8 s — the router log line `GraphQL endpoint exposed` had fired but the axum server task had not yet been polled when the first `execute_query` POST arrived, so the kernel RST'd the connection.
- `test_subscription_callback_error_payload` (CircleCI build 377898, 2026-05-22) panicked at `tests/integration/subscriptions/callback.rs:169:5` with `router at http://127.0.0.1:40031/ did not accept HTTP requests within 30s`. This test already had the `wait_for_router_ready` HEAD probe added in a prior bundle commit, but its 30 s deadline was exhausted under heavy CI contention — the accept loop took longer than 30 s to be polled.

Same root cause as the earlier `_error_payload` fix: `router.assert_started()` only waits for the `GraphQL endpoint exposed` log emitted in `axum_factory::axum_http_server_factory::create` immediately after `TcpListener::bind` resolves, BEFORE the spawned axum server task is actually polled. Under flake-bash 10x parallel contention the gap can be either short enough to fail with a connection reset (`_pure_error_payload`) or long enough to outrun a 30 s deadline (`_error_payload`).

The fix is twofold and matches the helper added in commit 836eaf683:

- Apply the existing `wait_for_router_ready` HEAD probe to `_pure_error_payload` between `assert_started().await` and the first `execute_query`, so the test only sends its POST once the accept loop is actually serving connections.
- Widen the `_error_payload` deadline from 30 s to 60 s. The probe only burns time when something is wrong; the bundle's bounded `dump_stack_traces` (10 s) still protects against truly hung subprocesses. 60 s matches the existing slack in `wait_for_callbacks` plus the harness's `assert_shutdown` 20 s ceiling.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9497
