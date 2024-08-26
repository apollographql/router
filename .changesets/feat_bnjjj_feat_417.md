### Support new telemetry trace ID format ([PR #5735](https://github.com/apollographql/router/pull/5735))

The router supports a new UUID format for telemetry trace IDs.


The following formats are supported in router configuration for trace IDs:

* `open_telemetry`
* `hexadecimal`  (same as `opentelemetry`)
* `decimal`
* `datadog`
* `uuid` (may contain dashes)

You can configure router logging to display the formatted trace ID with `display_trace_id`:

```yaml
 telemetry:
  exporters:
    logging:
      stdout:
        format:
          json:
            display_trace_id: (true|false|open_telemetry|hexadecimal|decimal|datadog|uuid)
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5735