### Add automatic unit conversion for duration instruments with non-second units

The router now automatically converts duration measurements to match the configured unit for telemetry instruments.
Previously, duration instruments always recorded values in seconds regardless of the configured `unit` field.
When you specify units like `"ms"` (milliseconds), `"us"` (microseconds), or `"ns"` (nanoseconds),
the router automatically converts the measured duration to the appropriate scale.

**Supported units:**
- `"s"` - seconds (default)
- `"ms"` - milliseconds
- `"us"` - microseconds
- `"ns"` - nanoseconds

> [!NOTE]
> Use this feature only when you need to integrate with an observability platform that doesn't properly translate from source time units to target time units (for example, seconds to milliseconds). In all other cases, follow the OTLP convention that you "SHOULD" use seconds as the unit.

**Example:**
```yaml title="router.yaml"
telemetry:
  instrumentation:
    instruments:
      subgraph:
        acme.request.duration:
          value: duration
          type: histogram
          unit: ms # Values are now automatically converted to milliseconds
          description: "Metric to get the request duration in milliseconds"
```

By [Jon Christiansen](https://github.com/theJC) in https://github.com/apollographql/router/pull/8415
