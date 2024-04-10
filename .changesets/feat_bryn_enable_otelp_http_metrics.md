### Support exporting metrics via OTLP HTTP ([Issue #4559](https://github.com/apollographql/router/issues/4559))

In addition to exporting metrics via OTLP/gRPC, the router now supports exporting metrics via OTLP/HTTP. 

You can enable exporting via OTLP/HTTP by setting the `protocol` key to `http` in your `router.yaml`:

```
telemetry:
  exporters:
    metrics:
      otlp:
        enabled: true
        protocol: http
```

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/4842
