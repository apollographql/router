### Add `enabled` field for telemetry exporters ([PR #3952](https://github.com/apollographql/router/pull/3952))

Telemetry configuration now supports `enabled` on all exporters. This allows exporters to be disabled without removing them from the configuration and in addition allows for a more streamlined default configuration.

```diff
telemetry:
  tracing: 
    datadog:
+      enabled: true
    jaeger:
+      enabled: true
    otlp:
+      enabled: true
    zipkin:
+      enabled: true
```

Existing configurations will be migrated to the new format automatically on startup. However, you should update your configuration to use the new format as soon as possible. 

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/3952
