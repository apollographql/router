### Remove the Jaeger trace propagator ([PR #9787](https://github.com/apollographql/router/pull/9787))

The `opentelemetry-jaeger-propagator` crate has deprecated the Jaeger propagation format in favor of W3C TraceContext propagation, and building against it now fails under `-D warnings`. Rather than suppress the warning, the Jaeger propagator has been removed entirely.

The `telemetry.exporters.tracing.propagation.jaeger` configuration option has been removed. If your router config sets `propagation: jaeger: true`, remove it and use `propagation: trace_context: true` instead:

```yaml
# Before (no longer supported)
telemetry:
  exporters:
    tracing:
      propagation:
        jaeger: true

# After
telemetry:
  exporters:
    tracing:
      propagation:
        trace_context: true
```

This only affects the propagation format used for distributed tracing headers; sending traces to Jaeger via the OTLP exporter is unaffected.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/9787
