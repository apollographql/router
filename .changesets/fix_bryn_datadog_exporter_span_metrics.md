### Datadog span metrics are now supported ([PR #5609](https://github.com/apollographql/router/pull/5609))

When using the APM view in Datadog, span metrics will be displayed for any span that was a top level span or has the `_dd.measured` flag set.

Apollo Router now sets the `_dd.measured` flag by default for the following spans:

* `request`
* `router`
* `supergraph`
* `subgraph`
* `subgraph_request`
* `http_request`
* `query_planning`
* `execution`
* `query_parsing`

You can override this behaviour to enable or disable span metrics for any span by setting the `span_metrics` configuration in the Datadog exporter configuration.

```yaml
telemetry:
  exporters:
    tracing:
      datadog:
        enabled: true
        span_metrics:
          # Disable span metrics for supergraph
          supergraph: false
          # Enable span metrics for my_custom_span
          my_custom_span: true
```

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5609
