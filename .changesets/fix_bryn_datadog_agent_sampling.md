### Enable accurate Datadog APM metrics ([PR #6017](https://github.com/apollographql/router/pull/6017))

The router supports a new preview feature, the `preview_datadog_agent_sampling` option, to enable sending all spans to the Datadog Agent so APM metrics and views are accurate.

Previously, the sampler option in `telemetry.exporters.tracing.common.sampler` wasn't Datadog-aware. To get accurate Datadog APM metrics, all spans must be sent to the Datadog Agent with a `psr` or `sampling.priority` attribute set appropriately to record the sampling decision.

The `preview_datadog_agent_sampling` option enables accurate Datadog APM metrics. It should be used when exporting to the Datadog Agent, via OTLP or Datadog-native. 

```yaml
telemetry:
  exporters:
    tracing:
      common:
        # Only 10 percent of spans will be forwarded from the Datadog agent to Datadog. Experiment to find a value that is good for you!
        sampler: 0.1
        # Send all spans to the Datadog agent. 
        preview_datadog_agent_sampling: true

        
```

Using these options can decrease your Datadog bill, because you will be sending only a percentage of spans from the Datadog Agent to Datadog. 

> [!IMPORTANT]
> Users must enable `preview_datadog_agent_sampling` to get accurate APM metrics. Users that have been using recent versions of the router will have to modify their configuration to retain full APM metrics. 

> [!IMPORTANT]
> The router doesn't support [`in-agent` ingestion control](https://docs.datadoghq.com/tracing/trace_pipeline/ingestion_mechanisms/?tab=java#in-the-agent).
> Configuring `traces_per_second` in the Datadog Agent won't dynamically adjust the router's sampling rate to meet the target rate.

> [!IMPORTANT]
> Sending all spans to the Datadog Agent may require that you tweak the `batch_processor` settings in your exporter config. This applies to both OTLP and Datadog native exporters.

Learn more by reading the [updated Datadog tracing documentation](https://apollographql.com/docs/router/configuration/telemetry/exporters/tracing/datadog) for more information on configuration options and their implications.