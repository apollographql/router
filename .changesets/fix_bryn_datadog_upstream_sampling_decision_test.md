### Respect x-datadog-sampling-priority ([PR #6017](https://github.com/apollographql/router/pull/6017))

This PR consists of two fixes:
#### Datadog priority sampling resolution is not lost.

Previously a `x-datadog-sampling-priority` of `-1` would be converted to `0` for downstream requests and `2` would be converted to `1`.

#### The sampler option in the `telemetry.exporters.tracing.common.sampler` is not datadog aware.

To get accurate APM metrics all spans must be sent to the datadog agent with a `psr` or `sampling.priority` attribute set appropriately to record the sampling decision.

`datadog_agent_sampling` option in the router.yaml enables this behavior and should be used when exporting to the datadog agent via OTLP. 
It is automatically enabled for the Datadog native exporter.

```yaml
telemetry:
  exporters:
    tracing:
      common:
        # Only 10 percent of spans will be forwarded from the Datadog agent to Datadog. Experiment to find a value that is good for you!
        sampler: 0.1
        # Send all spans to the Datadog agent. 
        datadog_agent_sampling: true
      
      # Example OTLP exporter configuration
      otlp:
        enabled: true
        # Optional batch processor setting, this will enable the batch processor to send concurrent requests in a high load scenario.
        batch_processor:
          max_concurrent_exports: 100

      # Example Datadog native exporter configuration 
      datadog:
        enabled: true
        
        # Optional batch processor setting, this will enable the batch processor to send concurrent requests in a high load scenario.
        batch_processor:
          max_concurrent_exports: 100
```

By using these options, you can decrease your Datadog bill as you will only be sending a percentage of spans from the Datadog agent to datadog. 

> [!IMPORTANT]
> Users using OTLP exporter must enable `datadog_agent_sampling` to get accurate APM metrics.

> [!IMPORTANT]
> Sending all spans to the datadog agent may require that you tweak the `batch_processor` settings in your exporter config. This applies to both OTLP and the Datadog native exporter.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/6017
