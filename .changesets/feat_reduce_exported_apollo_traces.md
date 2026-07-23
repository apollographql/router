### Reduce the volume of traces sent to Apollo

The Router now caps the volume of traces it exports to Apollo (GraphOS) with a throttle that runs after the configured trace samplers. This helps to avoid the processing and transfer of traces that would be dropped by GraphOS at ingestion time anyway. A new option, `telemetry.apollo.tracing.throttle`, selects the strategy:

- `representative_traces` (default): Sends at most one representative trace per minute for each distinct combination of operation, client, latency bucket, error status, and operation type. Duplicate traces that share a combination already seen in the current minute are dropped. Error traces are retained at a higher rate. This mirrors the representative-trace filtering used by Apollo's engine reports pipeline.
- `rate_limited`: Sends every trace but caps the export rate at a fixed maximum of 100 traces per second per Router instance. This is cheaper in CPU and memory than `representative_traces`. The rate is not configurable.

This throttle applies only to the trace pipeline that exports to Apollo Studio, and only to whatever the existing head samplers (`telemetry.apollo.sampler` and `telemetry.exporters.tracing.common.sampler`) already let through — it can only further reduce that traffic. Traces exported to a customer's own OTLP collector or to Datadog are unaffected. The existing `telemetry.apollo.sampler` continues to work as before, and to disable Apollo trace export entirely, set it to `always_off`.

By [@bonnici](https://github.com/bonnici) in https://github.com/apollographql/router/pull/9848
