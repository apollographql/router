### Split Apollo trace/metrics exporter configs ([PR #8258](https://github.com/apollographql/router/pull/8258))
The config related to the exporting of Apollo metrics and traces has been separated so that the various configuration can be fine-tuned for each of the Apollo exporters. The config has changed from:

```yaml
telemetry:
  apollo:
    batch_processor:
      scheduled_delay: 5s
      max_export_timeout: 30s
      max_export_batch_size: 512
      max_concurrent_exports: 1
      max_queue_size: 2048
```

To:

```yaml
telemetry:
  apollo:
    traces:
      # Config for Apollo OTLP traces (used if otlp_tracing_sampler is greater than 0).
      otlp:
        exporter:
          max_export_timeout: 130s
          scheduled_delay: 5s
          max_export_batch_size: 512
          max_concurrent_exports: 1
          max_queue_size: 2048
      # Config for Apollo usage report traces (used if otlp_tracing_sampler is less than 1).
      usage_reports:
        exporter:
          max_export_timeout: 30s
          scheduled_delay: 5s
          max_queue_size: 2048
        
    metrics:
      # Config for Apollo OTLP metrics. 
      otlp:
        exporter:
          scheduled_delay: 13s # This does not apply config gauge metrics, which have a non-configurable scheduled_delay.
          max_export_timeout: 30s
      # Config for Apollo usage report metrics.
      usage_reports:
        exporter:
          max_export_timeout: 30s
          scheduled_delay: 5s
          max_queue_size: 2048
```

The old telemetry.apollo.batch_processor config will be used if these new config values are not specified. The configuration used will be shown in an info-level log on router startup.

By [@bonnici](https://github.com/bonnici) in https://github.com/apollographql/router/pull/8258
