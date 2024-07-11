### Correct broken Datadog trace exporter span names and resource mapping ([Issue #5282](https://github.com/apollographql/router/issues/5282))

The Router has two ways of sending traces to Datadog:

1. The [OpenTelemetry for Datadog](https://www.apollographql.com/docs/router/configuration/telemetry/exporters/tracing/datadog/#otlp-configuration) approach (which is the recommended method).  This is identified by `otlp` in YAML configuration, and is *not* impacted by this fix; and
2. The ["Datadog native" configuration](https://www.apollographql.com/docs/router/configuration/telemetry/exporters/tracing/datadog/#datadog-native-configuration).  This is identified by the use of a `datadog:` key in YAML configuration.

This fixes a bug in the latter approach which caused a broken user experience in certain Datadog experiences, such as the "Resources" section of the [Datadog APM Service Catalog](https://docs.datadoghq.com/service_catalog/) page.

We now use static span names by default, with resource mappings providing additional context when desired, which enables the desired behavior which was not possible before.

_If for some reason you wish to maintain the existing behavior, you must either update your spans and resource mappings, or keep your spans and instead configure the router to use dynamic span names and disable resource mapping._

Enabling resource mapping and fixed span names is configured by the `enable_span_mapping` and `fixed_span_names` options:

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

With `enable_span_mapping` set to `true` (now default), the following resource mappings are applied:

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
telemetry:
  exporters:
    tracing:
      datadog:
        enabled: true
        resource_mapping:
          # Use `my.span.attribute` as the resource name for the `router` span
          router: "my.span.attribute"
```

To learn more, see the [Datadog trace exporter](https://www.apollographql.com/docs/router/configuration/telemetry/exporters/tracing/datadog) documentation.

By [@bnjjj](https://github.com/bnjjj) and [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/5386
