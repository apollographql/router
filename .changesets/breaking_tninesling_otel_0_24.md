### Upgrade OTel to support Hyper 1 ([PR #6493](https://github.com/apollographql/router/pull/6493))

Upgrades OpenTelemetry dependencies to support hyper 1. The latest versions of OpenTelemetry do not support legacy Jaeger traces. Users should export OpenTelemetry Protocol (OTLP) traces to Jaeger instead. Note that Jaeger _propagation_ is still supported.

(Deprecated) Legacy Jaeger Exporter:

```yaml
telemetry.exporters.tracing:
  propagation:
    jaeger: true
  jaeger:
    enabled: true
    batch_processor:
      scheduled_delay: 100ms
    agent:
      endpoint: default
```

(Recommended) OTLP Exporter:

```yaml
telemetry.exporters.tracing:
  propagation:
    jaeger: true
  otlp:
    enabled: true
    batch_processor:
      scheduled_delay: 100ms
    endpoint: default
```

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/6493
