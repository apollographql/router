### Fix Linux flake in `apollo_reports::test_metrics_with_library_name_http_header` (products subgraph ECONNRESET)

`apollo_reports::test_metrics_with_library_name_http_header` flaked on CircleCI's ARM Linux executor whenever the public Apollo demo subgraphs (`https://*.demo.starstuff.dev/`) reset the TLS connection mid-request. The router surfaced the failure as `SubrequestHttpError { service: "products", reason: "Connection reset by peer (os error 104)" }`, which then turned the recorded metrics shape (no subgraph errors, full `Product`/`Review`/`User` field counts) into a `topProducts: null` error payload, drifting the snapshot. See CircleCI job 378550 for the captured log.

**Root cause**

`tests/fixtures/supergraph.graphql` hardcodes the live demo subgraph URLs (e.g. `@join__graph(name: "products", url: "https://products.demo.starstuff.dev/")`), and the existing `get_metrics_report` helper opts into real network egress via `with_subgraph_network_requests()`. The test therefore made real HTTPS calls to a third-party host every run; an `ECONNRESET` from that host was indistinguishable (to the snapshot) from a router bug. Same flake mode as ROUTER-1814 in the sibling `apollo_otel_traces` binary.

**What changed**

Ported the wiremock pattern from `apollo_otel_traces` (`start_demo_subgraphs_mock_server` + `get_router_service_with_subgraph_mock`) into `apollo_reports.rs`, adapted for the `Report` collector and the `get_router_service` signature. Added a companion `get_metrics_report_with_subgraph_mock` and switched `test_metrics_with_library_name_http_header` over to it. The mock serves canned federation responses for `accounts`, `products`, and `reviews` at distinct paths and the router rewrites the hardcoded `https://*.demo.starstuff.dev/` URIs via `override_subgraph_url`. The FTV1 bytes are redacted by `assert_report!`, so the existing snapshot still matches.

Scope is intentionally narrow: only `test_metrics_with_library_name_http_header` is migrated. The same flake mode applies to the five sibling `test_metrics_with_*_http_header` / `test_metrics_with_*_request_extension` tests and other `get_metrics_report` / `get_trace_report` callers in this file; those will be addressed in follow-up tickets.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9497
