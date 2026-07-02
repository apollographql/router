### Fix Linux flake in four `apollo_reports::test_metrics_with_*` siblings (products subgraph ECONNRESET)

Four sibling `test_metrics_with_*` tests in `apollo-router/tests/apollo_reports.rs` flaked on CircleCI's AMD Linux executor (build 376289, prep-2.14.1, 2026-05-20) whenever the public Apollo demo subgraphs (`https://*.demo.starstuff.dev/`) reset the TLS connection mid-request. The router surfaced the failure as `SubrequestHttpError { service: "products", reason: "Connection reset by peer (os error 104)" }`, which then turned the recorded metrics shape into a `topProducts: null` error payload, drifting the snapshots.

**Root cause**

Identical to ROUTER-1823 (`test_metrics_with_library_name_http_header`): `tests/fixtures/supergraph.graphql` hardcodes live demo subgraph URLs, and `get_metrics_report` routes through `with_subgraph_network_requests()`, so the tests make real HTTPS calls to a third-party host every run. The four sibling tests share the same exposure.

**What changed**

Migrated the four remaining `test_metrics_with_*` siblings to the wiremock-backed `get_metrics_report_with_subgraph_mock` helper added in ROUTER-1823. Call signatures were identical — the helper was directly usable, no extension required. Tests migrated:

- `test_metrics_with_client_version_http_header`
- `test_metrics_with_library_version_http_header`
- `test_metrics_with_library_name_request_extension`
- `test_metrics_with_library_version_request_extension`

Scope is intentionally narrow: only the four `test_metrics_with_*` siblings exposed to the same flake mode are migrated. Other `get_metrics_report` / `get_trace_report` callers in this file (trace family, stats, persisted query variants) will be addressed in follow-up tickets if/when they flake.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9497
