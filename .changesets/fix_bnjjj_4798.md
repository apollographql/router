### Fix(telemetry): keep consistency between tracing otlp endpoint ([Issue #4798](https://github.com/apollographql/router/issues/4798))

In our [documentation](https://www.apollographql.com/docs/router/configuration/telemetry/exporters/tracing/otlp/#endpoint) when configuration http endpoint for tracing otlp exporter we say that users should only include the base address of the otlp endpoint. It was only working for grpc protocol and not http due to [this bug](https://github.com/open-telemetry/opentelemetry-rust/issues/1618). This inconsistency is now fixed with a workaround in the router waiting for the fix in openetelemetry crate.

So for example you'll have to specify the right path for http:

```yaml
telemetry:
  exporters:
    tracing:
      otlp:
        enabled: true
        endpoint: "http://localhost:4318"
        protocol: http
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/4801