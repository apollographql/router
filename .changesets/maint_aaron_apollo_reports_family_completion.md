### Close apollo_reports family flake — finish migrating remaining live-network callers ([Issue/PR #9497](https://github.com/apollographql/router/pull/9497))

Three follow-ups to ROUTER-1823 / ROUTER-1827 / ROUTER-1829 that surfaced in the 2026-05-28 last-24h CI sweep:

**B.** Migrated `test_persisted_query_by_id_stats` (`apollo_reports.rs:1199`) — the actual test that flaked with `Connection reset by peer` from `products.demo.starstuff.dev` on CircleCI build [379461](https://circleci.com/gh/apollographql/router/379461) — from `get_metrics_report` to `get_metrics_report_with_subgraph_mock`. Snapshot re-blessed against the wiremock's canonical FTV1 shape (gains per-field stats for `upc`).

**C.** Added `get_batch_router_service_with_subgraph_mock` + `get_batch_trace_report_with_subgraph_mock` helpers (mirrors the existing `*_with_subgraph_mock` pair). Migrated all 3 callers of `get_batch_trace_report` (`apollo_reports.rs:894`, `1088`, `1236`), including the actual test that flaked with `502 Bad Gateway` from `reviews.demo.starstuff.dev` on CircleCI build [379415](https://circleci.com/gh/apollographql/router/379415) — `test_demand_control_trace_batched`. Deleted the now-dead `get_batch_trace_report` helper.

**D.** Migrated the last `get_trace_report` caller (`test_condition_if` at line 819) from `get_trace_report` to `get_trace_report_with_subgraph_mock`, re-blessing `apollo_reports__condition_if(-2).snap` to reflect the wiremock's canonical field-ordering + type-info shape. Deleted the now-dead `get_trace_report` helper.

After this lands, **only `get_metrics_report` and `get_batch_metrics_report` callers remain on the live demo subgraphs** — those are tracked as a follow-up in POST-CLOSEOUT-CI-WATCH.md.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9497
