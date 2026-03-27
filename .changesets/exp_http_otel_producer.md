### Add support for sending Apollo OTel metrics and traces via HTTP ([PR #9055](https://github.com/apollographql/router/pull/9055))

Adds experimental support for sending Apollo OTLP metrics and traces via HTTP. This can be enabled using the config values:
- telemetry.apollo.experimental_otlp_tracing_protocol
- telemetry.apollo.experimental_otlp_metrics_protocol

GRPC is still the preferred method of sending OTLP metrics to Apollo, but we are adding this to support customers who cannot use GRPC.

By [@bonnici](https://github.com/bonnici) in https://github.com/apollographql/router/pull/9055
