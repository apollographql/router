### Allow configuring maximum cardinality for user-facing metrics ([PR #9220](https://github.com/apollographql/router/pull/9220))

Users can now set a `cardinality_limit` in their router config to override the OpenTelemetry SDK's default limit of 2000 distinct attribute combinations per metric. Once the limit is reached, additional attribute combinations are dropped and replaced with a single overflow series tagged `otel_metric_overflow="true"`, losing their per-attribute breakdown.

Note that raising the cardinality limit increases memory usage proportionally, since each allowed attribute combination consumes memory. Monitor `apollo.router.telemetry.metrics.cardinality_overflow` to detect when a metric is hitting its limit.

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

**Behavior change for existing `views[]` entries on non-histogram instruments.** Previously, any `views[]` entry without an explicit `aggregation` silently converted counters and gauges to histograms. A counter named `my.counter` with a per-view entry would emit `my_counter_bucket`/`my_counter_sum`/`my_counter_count` instead of `my_counter_total`. Per-view configuration (`cardinality_limit`, `rename`, `description`, `unit`, `allowed_attribute_keys`) now preserves the instrument's native aggregation.

If you were relying on this conversion (e.g., you have dashboards or alerts built on `_bucket`/`_sum`/`_count` series for a counter), add an explicit `aggregation: histogram` to the affected view to keep the previous behavior:

```yaml
views:
  - name: my.counter
    aggregation:
      histogram:
        buckets: [0.1, 0.5, 1.0]
```

By [@rregitsky](https://github.com/rossregitsky) in https://github.com/apollographql/router/pull/9220
