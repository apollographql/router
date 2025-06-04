### Telemetry: export properly resources on metrics configured on prometheus ([PR #7394](https://github.com/apollographql/router/pull/7394))

When configuring `telemetry.exporters.metrics.common.resource` to globally add labels on metrics, these labels were not exported to prometheus. This is now fixed.

```yaml
telemetry:
  exporters:
    metrics:
      common:
        resource:
          "test-resource": "test"
      prometheus:
        enabled: true
```

This bug only occurred with Prometheus and not OTLP.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/7394
