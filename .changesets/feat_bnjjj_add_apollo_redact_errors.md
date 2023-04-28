### Adds a configuration to redact errors in traces sent to Apollo Studio

If you want to redact errors coming from traces from your subgraphs and sent to Apollo Studio you can now set `tracing.apollo.field_level_instrumentation.redact_errors` to `true`.
And the configuration to configure the sampling for these traces move from `tracing.apollo.field_level_instrumentation_sampler` to `tracing.apollo.field_level_instrumentation.sampler`.

Example:

```yaml
telemetry:
  apollo:
    field_level_instrumentation:
      # This example will trace half of requests. This number can't
      # be higher than tracing.trace_config.sampler.
      sampler: 0.5
      # Redact errors sent to Studio
      redact_errors: true # (default: false)
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3011