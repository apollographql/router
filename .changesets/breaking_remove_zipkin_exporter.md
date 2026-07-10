### Remove the Zipkin trace exporter and propagator ([PR #9787](https://github.com/apollographql/router/pull/9787))

The `opentelemetry-zipkin` crate has deprecated its exporter and propagator in favor of OTLP (Zipkin supports OTLP ingestion via [zipkin-otel](https://github.com/openzipkin-contrib/zipkin-otel)), and building against it now fails under `-D warnings`. Rather than suppress the warning, the native Zipkin exporter and propagator have been removed entirely.

The `telemetry.exporters.tracing.zipkin` exporter config and the `telemetry.exporters.tracing.propagation.zipkin` propagation option have both been removed. Configure the OTLP exporter to send traces to Zipkin instead:

```yaml
# Before (no longer supported)
telemetry:
  exporters:
    tracing:
      zipkin:
        enabled: true
        endpoint: "http://127.0.0.1:9411/api/v2/spans"

# After
telemetry:
  exporters:
    tracing:
      otlp:
        enabled: true
        endpoint: "http://127.0.0.1:9411"
        protocol: http
```

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/9787
