### Automatic unit conversion for duration instruments with non-second units

The Router now automatically converts duration measurements to match the configured unit for telemetry instruments.
Previously, duration instruments always recorded values in seconds regardless of the configured `unit` field.
With this enhancement, when you specify units like `"ms"` (milliseconds), `"us"` (microseconds), or `"ns"` (nanoseconds),
the Router will automatically convert the measured duration to the appropriate scale.

**Supported units:**
- `"s"` - seconds (default)
- `"ms"` - milliseconds
- `"us"` - microseconds
- `"ns"` - nanoseconds

Important Note: This should only be used in cases where you are required to integrate with an observability platform that does not properly translate from source timeunit into the necessary target timeunit (ie: seconds to milliseconds).  In all other cases, 
customers should follow the OLTP convention indicating you "SHOULD" use seconds as the unit.

**Example:**
```yaml title="router.yaml"
telemetry:
  instrumentation:
    instruments:
      subgraph:
        mycompany.http.client.request.duration:
          unit: "ms"  # Values are now automatically converted to milliseconds
```

By [Jon Christiansen](https://github.com/theJC) in https://github.com/apollographql/router/pull/8415
