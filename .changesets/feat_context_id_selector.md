### Add `context_id` selector for telemetry to expose unique per-request identifier ([GRAPHOS-67](https://apollographql.atlassian.net/browse/GRAPHOS-67))

A new `context_id` selector is now available for router, supergraph, and subgraph telemetry instrumentation. This selector exposes the unique per-request context ID that can be used to reliably correlate and debug requests in traces, logs, and custom events.

Previously, the context ID was only accessible via Rhai scripts as `request.id`, but Rhai runs after telemetry, preventing its use in telemetry attributes. With this change, users can now include the context ID in spans and other telemetry data.

Example configuration:

```yaml
telemetry:
  instrumentation:
    spans:
      router:
        attributes:
          "request.id":
            context_id: true
      supergraph:
        attributes:
          "request.id":
            context_id: true
      subgraph:
        attributes:
          "request.id":
            context_id: true
```

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/8899
