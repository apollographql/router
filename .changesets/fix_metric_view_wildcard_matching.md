### Metric view names support glob patterns again ([PR #9809](https://github.com/apollographql/router/pull/9809))

Glob patterns (`*`, `?`, `[...]`) and catch-all (`*` or an empty string) patterns in `telemetry.exporters.metrics.common.views[].name` match instruments again.

Since v2.13.0 only exact names matched, so a view targeting several instruments at once no longer worked. For example, custom histogram buckets on a `cost.*` view were not applied, and a catch-all drop rule stopped suppressing anything:

```yaml
telemetry:
  exporters:
    metrics:
      common:
        views:
          - name: cost.*            # custom buckets now apply to cost.estimated, cost.actual, ...
            aggregation:
              histogram:
                buckets: [0, 10, 100, 1000]
          - name: "*"               # this catch-all now drops everything not matched above
            aggregation: drop
```

Views are evaluated in declaration order and the first matching view wins, so you can place more-specific views before wildcards to control precedence.

By [@apollo-mateuswgoettems](https://github.com/apollo-mateuswgoettems) in https://github.com/apollographql/router/pull/9809
