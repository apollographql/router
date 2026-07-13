### Remove the native Zipkin trace exporter

The native Zipkin trace exporter (`telemetry.exporters.tracing.zipkin`) has been removed. Zipkin ingests OTLP, and OpenTelemetry has deprecated native Zipkin exporters in favor of OTLP.

If you export traces to Zipkin, configure the OTLP exporter to point at your Zipkin OTLP endpoint instead:

```yaml
# Before (no longer supported)
telemetry:
  exporters:
    tracing:
      zipkin:
        enabled: true
        endpoint: http://localhost:9411/api/v2/spans

# After
telemetry:
  exporters:
    tracing:
      otlp:
        enabled: true
        endpoint: http://localhost:9411 # your Zipkin OTLP endpoint
```

Zipkin (B3) trace-context propagation (`telemetry.exporters.tracing.propagation.zipkin`) is unaffected and remains supported.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/ROUTER-1657
