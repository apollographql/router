### Deprecate `apollo_router::otel_compat`

`otel_compat::HeaderExtractor` and `otel_compat::HeaderInjector` are now deprecated. The `opentelemetry_http` crate (v0.31+) already ships identical types that work with `http` 1.x.

Use `opentelemetry_http::HeaderExtractor` and `opentelemetry_http::HeaderInjector` directly. These types will be removed in a future major version.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9573
