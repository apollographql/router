### Respect x-datadog-sampling-priority ([PR #6017](https://github.com/apollographql/router/pull/6017))

This PR consists of two fixes:
#### Datadog priority sampling resolution is not lost.

Previously a `x-datadog-sampling-priority` of `-1` would be converted to `0` for downstream requests and `2` would be converted to `1`.

#### The sampler option in the `telemetry.exporters.tracing.common.sampler` is not datadog aware.

To get accurate APM metrics all spans must be sent to the datadog agent with a `psr` or `sampling.priority` attribute set appropriately to record the sampling decision.

`datadog_agent_sampling` option in the router.yaml enables this behavior and should be used when exporting to the datadog agent via otlp. 
It is automatically enabled for the Datadog native exporter.

```yaml
telemetry:
  exporters:
    tracing:
      common:
        # Only 10 percent of spans will be sent to the Datadog agent with `psr` or `sampling.priority` 1.
        sampler: 0.1
        # Send all spans to the Datadog agent. 
        datadog_agent_sampling: true
```

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/6017
