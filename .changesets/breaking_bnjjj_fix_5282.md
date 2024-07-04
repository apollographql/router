### Datadog exporter resource behavior and mapping ([Issue #5282](https://github.com/apollographql/router/issues/5282))

Users of the Datadog trace exporter may have noticed that span and resource naming is not as expected. 
Unlike other APMs, Datadog expects static span names, and then uses resource mapping to provide additional context.

The default behavior of the Datadog exporter has now been changed to support this and give a better user experience.

```yaml
telemetry:
  exporters:
    tracing:
      datadog:
        enabled: true
        # Enables resource mapping, previously disabled by default, but now enabled.
        enable_span_mapping: true
        # Enables fixed span names, defaults to true.
        fixed_span_names: true

  instrumentation:
    spans:
      mode: spec_compliant
            
```

The following default resource mappings are applied:

| OpenTelemetry Span Name | Datadog Span Operation Name |
|-------------------------|-----------------------------|
| `request`               | `http.route`                |
| `router`                | `http.route`                |
| `supergraph`            | `graphql.operation.name`    |
| `query_planning`        | `graphql.operation.name`    |
| `subgraph`              | `subgraph.name`             |
| `subgraph_request`      | `graphql.operation.name`    |
| `http_request`          | `http.route`                |

You can override the default resource mappings by specifying the `resource_mapping` configuration:

```yaml
  exporters:
    tracing:
      datadog:
        enabled: true
        resource_mapping:
          # Use `my.span.attribute` as the resource name for the `router` span
          router: "my.span.attribute"
```

By [@bnjjj](https://github.com/bnjjj) and [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/5386
