### Add OTLP http metrics export ([Issue #4559](https://github.com/apollographql/router/issues/4559))

Users can now export metrics via OTLP Http in addition to the existing OTLP Grpc

Activate this by setting the `protocol` to `http` in your  your `router.yaml`:

```
telemetry:
  exporters:
    metrics:
      otlp:
        enabled: true
        protocol: http
```

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/4842
