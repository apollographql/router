### Report Datadog exporter sub-feature configuration in config metrics

The `apollo.router.config.telemetry` gauge now reports which Datadog-specific telemetry options are explicitly configured, alongside the existing `opt.tracing.datadog` attribute:

- `opt.tracing.datadog.enable_span_mapping`
- `opt.tracing.datadog.fixed_span_names`
- `opt.tracing.datadog.resource_mapping`
- `opt.tracing.datadog.span_metrics`
- `opt.tracing.datadog.sampler`
- `opt.tracing.common.preview_datadog_agent_sampling`
- `opt.tracing.propagation.datadog`

This helps us understand how the native Datadog exporter's features are used so we can plan its documented deprecation in favor of OTLP, and shape the corresponding migration guidance.

By [@SharkBaitDLS](https://github.com/SharkBaitDLS) in https://github.com/apollographql/router/pull/10125
