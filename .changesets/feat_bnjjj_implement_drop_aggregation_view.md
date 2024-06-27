### Add the ability to drop metrics using otel views ([PR #5531](https://github.com/apollographql/router/pull/5531))

You can drop specific metrics if you don't want these metrics to be sent to your APM using [otel views](https://opentelemetry.io/docs/specs/otel/metrics/sdk/#view).

```yaml title="router.yaml"
telemetry:
  exporters:
    metrics:
      common:
        service_name: apollo-router
        views:
          - name: apollo_router_http_request_duration_seconds # Instrument name you want to edit. You can use wildcard in names. If you want to target all instruments just use '*'
            aggregation: drop

```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5531