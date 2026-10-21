### Remove the legacy Apollo protobuf trace export path

Router 3.0 exports traces to GraphOS exclusively via OTLP. The legacy protobuf-over-HTTP trace export and the `telemetry.apollo.otlp_tracing_sampler` option that selected between the two transports have been removed. Usage report metrics are unaffected and continue to use the Apollo usage reporting protocol.

An automatic configuration migration deletes `telemetry.apollo.otlp_tracing_sampler` when upgrading to Router 3.x and logs a warning. To sample traces sent to GraphOS, use `telemetry.apollo.sampler` or the common tracing sampler instead.

By [@rregitsky](https://github.com/rregitsky) in https://github.com/apollographql/router/pull/9819
