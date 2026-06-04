### Configure independent sampling rates per tracing exporter ([PR #XXXX](https://github.com/apollographql/router/pull/XXXX))

You can now set a `sampler` on individual tracing exporters so that each exporter receives a different fraction of traces. Previously, `telemetry.exporters.tracing.common.sampler` applied globally and all exporters received the same set of spans.

The per-exporter `sampler` field is available on:

- `telemetry.exporters.tracing.otlp.sampler`
- `telemetry.exporters.tracing.zipkin.sampler`
- `telemetry.exporters.tracing.datadog.sampler`
- `telemetry.apollo.sampler`

The value uses the same trace-ID-based algorithm as `telemetry.exporters.tracing.common.sampler`, so it represents an absolute fraction of all requests — not a fraction of already-sampled spans. For example, to send 10% of traces to Apollo Studio but only 2% to an external OTLP endpoint:

```yaml
telemetry:
  exporters:
    tracing:
      common:
        sampler: 0.1
      otlp:
        enabled: true
        endpoint: ${env.DISTRIBUTED_TRACING_ENDPOINT}
        sampler: 0.02
```

The per-exporter `sampler` must not exceed `telemetry.exporters.tracing.common.sampler`; Router returns an error at startup if it does.

The `sampler` field is ignored on the Datadog exporter when `preview_datadog_agent_sampling` is enabled, because in that mode the Datadog agent controls sampling decisions and all spans must be forwarded unfiltered.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/XXXX
