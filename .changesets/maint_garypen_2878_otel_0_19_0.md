### update opentelemetry to 0.19.0 ([Issue #2878](https://github.com/apollographql/router/issues/2878))


We've updated the following opentelemetry related crates:

```
opentelemetry 0.18.0 -> 0.19.0
opentelemetry-datadog 0.6.0 -> 0.7.0
opentelemetry-http 0.7.0 -> 0.8.0
opentelemetry-jaeger 0.17.0 -> 0.18.0
opentelemetry-otlp 0.11.0 -> 0.12.0
opentelemetry-semantic-conventions 0.10.0 -> 0.11.0
opentelemetry-zipkin 0.16.0 -> 0.17.0
opentelemetry-prometheus 0.11.0 -> 0.12.0
tracing-opentelemetry 0.18.0 -> 0.19.0
```

This allows us to close a number of opentelemetry related issues.

Note:

The prometheus specification mandates naming format and, unfortunately, the router had two metrics which weren't compliant. The otel upgrade enforces the specification, so the affected metrics are now renamed (see below).

The two affected metrics in the router were:

apollo_router_cache_hit_count -> apollo_router_cache_hit_count_total
apollo_router_cache_miss_count -> apollo_router_cache_miss_count_total

If you are monitoring these metrics via prometheus, please update your dashboards with this name change.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3421
