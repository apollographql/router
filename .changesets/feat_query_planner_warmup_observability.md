### Add observability metrics for query planner warm-up ([PR #XXXX](https://github.com/apollographql/router/pull/XXXX))

Query planner warm-up previously emitted only `apollo.router.query_planning.warmup.duration`, so operators firing prewarm requests had no way to confirm warm-up completed or diagnose silent failures. Warm-up now emits three additional metrics:

- `apollo.router.query_planning.warmup.operations` - operations processed during warm-up, attributed by `outcome` and `source` (`persisted_query`, `cache`).
- `apollo.router.query_planning.warmup.operations.expected` - operations warm-up intended to plan, attributed by `source`. Together with `warmup.operations`, this lets you compute warm-up coverage (planned / expected).
- `apollo.router.query_planning.warmup.backpressure` - times warm-up retried after a temporary compute-backpressure error, attributed by `source` and `phase` (`parse`, `plan`).

The `outcome` attribute shares its vocabulary with `apollo.router.query_planning.plan.duration` (`success`, `timeout`, `cancelled`, `error`), plus `memory_limit` for query-planning memory-limit cancellations.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/XXXX
