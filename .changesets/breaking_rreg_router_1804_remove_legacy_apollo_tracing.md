### Remove the legacy Apollo protobuf trace export path

Router 3.0 exports traces to GraphOS exclusively via OTLP. The legacy protobuf-over-HTTP trace export and the `telemetry.apollo.otlp_tracing_sampler` option that selected between the two transports have been removed. Usage report metrics are unaffected and continue to use the Apollo usage reporting protocol.

An automatic configuration migration deletes `telemetry.apollo.otlp_tracing_sampler` (and its older `experimental_otlp_tracing_sampler` spelling) when upgrading to Router 3.x and logs a warning. To sample traces sent to GraphOS, use `telemetry.apollo.sampler` or the common tracing sampler instead.

Two behaviors specific to the legacy transport are also removed: the free-plan trace-suppression warning ("traces will not be sent to Apollo as this account is on a free plan") and the trace-specific retry. Trace export now relies on the OTLP batch processor's own retry. Reinstating a free-plan warning on the OTLP path is tracked in ROUTER-1999.

By [@rregitsky](https://github.com/rregitsky) in https://github.com/apollographql/router/pull/9819
