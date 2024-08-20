### Add support of other format for trace id in telemetry ([PR #5735](https://github.com/apollographql/router/pull/5735))

Currently we support datadog and otel traceID formats and decimal. However we would like to also support UUID.

Unify the two `TraceIdFormat` enums into a single enum that us used across selectors and experimental_expose_trace id.

Ensure the following formats are supported:

+ open_telemetry
+ hexadecimal  (same as opentelemetry)
+ decimal
+ datadog
+ uuid (this has dashes)

Add support for logging to output using `TraceIdFormat`

```yaml
 telemetry:
  exporters:
    logging:
      stdout:
        format:
          json:
            disaplay_trace_id: (true|false|open_telemetry|hexadecimal|decimal|datadog|uuid)
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5735