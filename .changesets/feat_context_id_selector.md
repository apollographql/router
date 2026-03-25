### Add `context_id` selector for telemetry to expose unique per-request identifier ([#8899](https://github.com/apollographql/router/pull/8899))

A new `context_id` selector is now available for router, supergraph, subgraph, and connector telemetry instrumentation. This selector exposes the unique per-request context ID that can be used to reliably correlate and debug requests in traces, logs, and custom events.

Previously, while the context ID was accessible in Rhai scripts as `request.id`, there was no telemetry selector to expose it. With this change, users can now include `context_id: true` in their telemetry configuration to add the context ID to spans, logs, and custom events.

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
      connector:
        attributes:
          "request.id":
            context_id: true
```

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/8899
