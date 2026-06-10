### Close `apollo_reports` trace-report family flake (8 of 9 callers sandboxed)

Two `apollo_reports` tests in the trace family flaked in the 2026-05-28 last-24h CI sweep against the public Apollo demo subgraphs:

- `test_persisted_query_by_id_stats` (CircleCI build 379461, ARM Linux): `Connection reset by peer (os error 104)` from `products.demo.starstuff.dev`
- `test_demand_control_trace_batched` (CircleCI build 379415, AMD Linux): `502: Bad Gateway` from `reviews.demo.starstuff.dev`

Same root-cause class as ROUTER-1814 / ROUTER-1823 / ROUTER-1827, deferred follow-up on those tickets: `apollo_reports::get_trace_report` still routed through `with_subgraph_network_requests()` against the live `https://*.demo.starstuff.dev/` hosts hardcoded in `tests/fixtures/supergraph.graphql`.

**What changed**

Added `get_trace_report_with_subgraph_mock` as a sibling of the existing `get_metrics_report_with_subgraph_mock` helper, using the same wiremock-backed `get_router_service_with_subgraph_mock` and the same canned subgraph responses (`start_demo_subgraphs_mock_server`). Migrated 8 of 9 `get_trace_report` call sites:

- `non_defer`, `test_condition_else`, `test_trace_id`, `test_trace_with_client_name_http_header`, `test_trace_with_client_version_http_header`, `test_send_header`, `test_send_variable_value`, `test_demand_control_trace`

**Why one site (`test_condition_if`) was left on the live helper**

The wiremock's canned FTV1 blob for the products subgraph emits the `Product` selection set in the order `upc`, `name`, which matches every other trace-family snapshot in this file (`non_defer`, `trace_id`, `condition_else`, etc.). The committed `apollo_reports__condition_if.snap` records the opposite order (`name`, `upc`) — a pre-existing inconsistency from the live demo subgraph's flaky field ordering at the time the snapshot was last captured. Migrating `test_condition_if` would have required re-blessing the snapshot, which is out of scope for a flake fix; flagged inline for a follow-up.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9497
