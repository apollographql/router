### Support Datadog span metrics ([PR #5609](https://github.com/apollographql/router/pull/5609))

When using the APM view in Datadog, the router now displays span metrics for top-level spans or spans with the `_dd.measured` flag set.

The router sets the `_dd.measured` flag by default for the following spans:

* `request`
* `router`
* `supergraph`
* `subgraph`
* `subgraph_request`
* `http_request`
* `query_planning`
* `execution`
* `query_parsing`

To enable or disable span metrics for any span, configure `span_metrics` for the Datadog exporter:

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

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5609 and https://github.com/apollographql/router/pull/5703
