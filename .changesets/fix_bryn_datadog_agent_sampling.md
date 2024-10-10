### Add `preview_datadog_agent_sampling` ([PR #6017](https://github.com/apollographql/router/pull/6017))

The sampler option in the `telemetry.exporters.tracing.common.sampler` is not datadog aware.

To get accurate APM metrics all spans must be sent to the datadog agent with a `psr` or `sampling.priority` attribute set appropriately to record the sampling decision.

`preview_datadog_agent_sampling` option in the router.yaml enables this behavior and should be used when exporting to the datadog agent via OTLP or datadog native. 

```yaml
telemetry:
  exporters:
    tracing:
      common:
        # Only 10 percent of spans will be forwarded from the Datadog agent to Datadog. Experiment to find a value that is good for you!
        sampler: 0.1
        # Send all spans to the Datadog agent. 
        preview_datadog_agent_sampling: true
      
      # Example OTLP exporter configuration
      otlp:
        enabled: true

      # Example Datadog native exporter configuration 
      datadog:
        enabled: true
        
```

By using these options, you can decrease your Datadog bill as you will only be sending a percentage of spans from the Datadog agent to datadog. 

> [!IMPORTANT]
> Users must enable `preview_datadog_agent_sampling` to get accurate APM metrics. Users that have been using recent versions of the router will have to modify their configuration to retain full APM metrics. 

> [!IMPORTANT]
> Sending all spans to the datadog agent may require that you tweak the `batch_processor` settings in your exporter config. This applies to both OTLP and the Datadog native exporter.

See the updated Datadog tracing documentation for more information on configuration options and their implications.