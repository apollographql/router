### Fix GRPC metadata configuration ([Issue #2831](https://github.com/apollographql/router/issues/2831))

Previously exporters that used GRPC as a protocol would not correctly parse the metadata configuration. Forcing the user to specify a list of values instead of a map.

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
This will now be parsed correctly and the user can specify a map of values:

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
