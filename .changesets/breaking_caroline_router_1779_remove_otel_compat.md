### Remove `apollo_router::otel_compat`

The `otel_compat` module has been removed. The `opentelemetry_http` crate (v0.31+) already ships identical `HeaderExtractor` and `HeaderInjector` types that work with `http` 1.x, so the router's copies were redundant.

If you used `apollo_router::otel_compat::HeaderExtractor` or `apollo_router::otel_compat::HeaderInjector`, replace them with `opentelemetry_http::HeaderExtractor` and `opentelemetry_http::HeaderInjector` respectively.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9572
