### Instrument experimental config features with OTLP gauges ([PR #9330](https://github.com/apollographql/router/pull/9330))

Adds `apollo.router.config.experimental_*` OTLP gauge metrics for all customer-facing experimental config flags, using the existing `populate_config_instrument!` pattern in `configuration/metrics.rs`. This enables usage tracking in the data warehouse so that adoption data can inform decisions about which experimental features to promote or remove in future releases.

The following features are now instrumented: `experimental_chaos`, `experimental_type_conditioned_fetching`, `experimental_hoist_orphan_errors`, `experimental_log_on_broken_pipe`, `experimental_plans_limit`, `experimental_paths_limit`, `experimental_reuse_query_plans`, `experimental_cooperative_cancellation`, `experimental_prewarm_query_plan_cache`, `experimental_local_field_metrics`, `experimental_response_trace_id`, and `experimental_otlp_endpoint`. Note that `experimental_http2` was already tracked as part of the existing `apollo.router.config.traffic_shaping` instrument.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9330
