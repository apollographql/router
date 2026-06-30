### Remove `telemetry.instrumentation.spans.mode: deprecated`

The `deprecated` span mode has been removed. It kept the old "request"-as-root-span span layout while the OpenTelemetry spec-compliant mode was being rolled out. Router 3.0 removes this option entirely.

If your config sets `mode: deprecated`, change it to `mode: spec_compliant` or remove the `mode` line (spec_compliant is the default):

```yaml
# Before (no longer supported)
telemetry:
  instrumentation:
    spans:
      mode: deprecated

# After
telemetry:
  instrumentation:
    spans: {}
```

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/ROUTER-1773
