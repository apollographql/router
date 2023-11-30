### Fix gRPC metadata configuration ([Issue #2831](https://github.com/apollographql/router/issues/2831))

Previously, telemetry exporters that used gRPC as a protocol would not correctly parse metadata configuration. Consequently, a user was forced to use a workaround of specifying a list of values instead of a map. For example:

```yaml
telemetry:
  exporters:
    tracing:
      otlp:
        grpc:
          metadata:
            "key1": "value1" # Failed to parse
            "key2":  # Succeeded to parse
              - "value2"
```

This issue has been fixed, and the following example with a map of values now parses correctly:

```yaml
telemetry:
  exporters:
    tracing:
      otlp:
        grpc:
          metadata:
            "key1": "value1"
```

By [@bryncooke](https://github.com/AUTHOR) in https://github.com/apollographql/router/pull/4285
