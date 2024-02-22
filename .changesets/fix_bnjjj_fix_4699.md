### Default header correctly set in `experimental_response_trace_id` when enabled ([Issue #4699](https://github.com/apollographql/router/issues/4699))

When configuring the `experimental_response_trace_id` without an explicit header it now correctly takes the default one `apollo-trace-id`.

Example of configuration:

```yaml
telemetry:
  exporters:
    tracing:
      experimental_response_trace_id:
        enabled: true
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/4702