### Promote `experimental_otlp_*` Apollo telemetry config fields

The three Apollo-specific OTLP settings under `telemetry.apollo` have been
promoted out of experimental. Rename them in your router configuration:

| Old field | New field |
| --- | --- |
| `telemetry.apollo.experimental_otlp_endpoint` | `telemetry.apollo.otlp_endpoint` |
| `telemetry.apollo.experimental_otlp_tracing_protocol` | `telemetry.apollo.otlp_tracing_protocol` |
| `telemetry.apollo.experimental_otlp_metrics_protocol` | `telemetry.apollo.otlp_metrics_protocol` |

```yaml
# Before (no longer supported)
telemetry:
  apollo:
    experimental_otlp_endpoint: https://usage-reporting.api.apollographql.com/
    experimental_otlp_tracing_protocol: grpc
    experimental_otlp_metrics_protocol: grpc

# After
telemetry:
  apollo:
    otlp_endpoint: https://usage-reporting.api.apollographql.com/
    otlp_tracing_protocol: grpc
    otlp_metrics_protocol: grpc
```

Default values are unchanged. Configurations using the old field names are
migrated automatically at startup.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/PR_NUMBER
