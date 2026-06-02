### Migrate `test_metrics_with_client_name_http_header` to subgraph mock ([Issue/PR #9497](https://github.com/apollographql/router/pull/9497))

Scope-miss fix on ROUTER-1827 / commit `1f50ccc51`. The PR description claimed "all 6 `apollo_reports::test_metrics_with_*` tests now sandboxed" but `test_metrics_with_client_name_http_header` was missed — it still called the live-network `get_metrics_report`. Same `ECONNRESET`-from-public-demo flake mode (ROUTER-1814 / ROUTER-1823 / ROUTER-1827) applied. One-line swap to `get_metrics_report_with_subgraph_mock`, matching the other five siblings.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9497
