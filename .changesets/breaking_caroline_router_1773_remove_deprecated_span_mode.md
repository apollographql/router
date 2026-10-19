### Remove `telemetry.instrumentation.spans.mode: deprecated`

The `deprecated` span mode has been removed. It kept the old "request"-as-root-span span layout while the OpenTelemetry spec-compliant mode was being rolled out. Router 3.0 removes this option entirely.

If your config sets `mode: deprecated`, remove it. If `spans:` has no other configuration, you can remove the `spans:` and `instrumentation:` keys too:

```yaml
# Before (no longer supported)
telemetry:
  instrumentation:
    spans:
      mode: deprecated

# After
telemetry:
```

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/ROUTER-1773
