### Custom OpenTelemetry Datadog exporter mapping ([Issue #2228](https://github.com/apollographql/router/issues/2228))

This PR fixes the issue with DD exporter not providing meaningful data in the DD traces.
There is a [known issue](https://docs.rs/opentelemetry-datadog/latest/opentelemetry_datadog/#quirks) where open telemetry is not fully compatible with datadog.

To fix, this, open-telemetry-datadog added [custom mapping functions](https://docs.rs/opentelemetry-datadog/0.6.0/opentelemetry_datadog/struct.DatadogPipelineBuilder.html#method.with_resource_mapping).

All this logic is gated behind a yaml configuration boolean `enable_span_mapping` which if enabled will take the values from the span attributes.

By [@samuelAndalon](https://github.com/samuelAndalon) in https://github.com/apollographql/router/pull/2790
