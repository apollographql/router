### Fix Linux flake in `apollo_otel_traces::test_send_variable_value` (accounts subgraph ECONNRESET)

`apollo_otel_traces::test_send_variable_value` flaked on CircleCI's Linux executor whenever the public Apollo demo subgraphs (`https://*.demo.starstuff.dev/`) reset the TLS connection mid-request. The router surfaced the failure as `SubrequestHttpError { service: "accounts", reason: "Connection reset by peer (os error 104)" }`, which then turned the `apollo.subgraph.name=accounts` `http_request` span's status from `code: 0` (OK) to `code: 2` (ERROR) and dropped the `apollo_private.ftv1` attribute — both of which the snapshot expects to be present and OK. See the original CircleCI job 377214 for the captured trace log.

**Root cause**

`tests/fixtures/supergraph.graphql` hardcodes the live demo subgraph URLs (e.g. `@join__graph(name: "accounts", url: "https://accounts.demo.starstuff.dev/")`), and the existing `get_router_service` helper opts into real network egress via `with_subgraph_network_requests()`. The test therefore made real HTTPS calls to a third-party host every run; an `ECONNRESET` from that host was indistinguishable (to the snapshot) from a router bug.

**What changed**

Introduced a localhost wiremock (`start_demo_subgraphs_mock_server`) that serves canned federation responses for the three demo subgraphs — `accounts`, `products`, `reviews` — at distinct paths, each returning a valid FTV1 trace blob captured from the live demo deployment. A companion helper, `get_router_service_with_subgraph_mock`, wires `override_subgraph_url` config into the harness so the router rewrites the hardcoded `https://*.demo.starstuff.dev/` URIs to the wiremock. The FTV1 bytes are redacted by `assert_report!`, so the existing snapshot still matches.

Scope is intentionally narrow to ROUTER-1814: only `test_send_variable_value` is migrated to the mock-backed path. The same flake mode applies to other tests in this file that go through `get_trace_report` (e.g. `non_defer`, `test_client_name`, `test_send_header`); those will be addressed in follow-up tickets.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9497
