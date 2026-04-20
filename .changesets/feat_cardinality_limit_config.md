### Allow configuring maximum cardinality for customer-facing metrics ([Issue #ROUTER-1671](https://apollographql.atlassian.net/browse/ROUTER-1671))

Customers can now set a `cardinality_limit` in their router config to override the OpenTelemetry SDK's default limit of 2000 distinct attribute combinations per metric. This is useful when high-cardinality attributes cause metrics to be silently dropped.

The limit can be set globally under `telemetry.exporters.metrics.common.cardinality_limit` and per-metric under individual `views[].cardinality_limit`. The per-metric setting takes precedence over the global one.

```yaml
telemetry:
  exporters:
    metrics:
      common:
        cardinality_limit: 5000
        views:
          - name: http.server.request.duration
            cardinality_limit: 20000
```

Per-view configuration (`cardinality_limit`, `rename`, `description`, `unit`, `allowed_attribute_keys`) now works correctly on non-histogram instruments. Previously, any `views[]` entry without an explicit `aggregation` silently converted counters and gauges to histograms.

By [@rossregitsky](https://github.com/rossregitsky) in https://github.com/apollographql/router/pull/PULL_NUMBER
