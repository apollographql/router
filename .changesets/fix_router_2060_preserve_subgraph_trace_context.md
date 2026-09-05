### Add an opt-in setting to preserve an existing trace-context header on outgoing subgraph requests ([PR #10014](https://github.com/apollographql/router/pull/10014))

Under the OTLP exporter, the router's outbound HTTP client unconditionally re-injected its own span context into `traceparent`/`tracestate` (and the custom trace ID header, if `propagation.request.header_name` is configured) on every subgraph request, discarding any value already present on that header — whether set by a coprocessor, a Rhai script, or header propagation from the original client request. This silently broke trace correlation for setups that deliberately rewrite these headers before the subgraph call, for example a coprocessor inserting itself as a hop in the trace (standard, spec-compliant W3C Trace Context practice) or forwarding a client-generated trace ID for RUM correlation. Under the Datadog-native exporter this didn't happen, but only because Datadog's own propagator never touches those headers — an incidental side effect of that exporter, not a documented guarantee.

A new setting, `telemetry.exporters.tracing.propagation.preserve_trace_context_on_subgraph_requests`, lets you opt into keeping whatever trace-context header is already present on a subgraph request instead of overwriting it:

```yaml
telemetry:
  exporters:
    tracing:
      propagation:
        preserve_trace_context_on_subgraph_requests: true
```

It defaults to `false`, so existing deployments are unaffected. When enabled, the router still performs its own trace-context injection exactly as before; it then restores whichever header was present beforehand, so every other propagator (baggage, Jaeger, Zipkin, Datadog, X-Ray) behaves identically to today regardless of this setting. Only one trace-ID-carrying header is preserved per call: the custom trace ID header takes priority when configured and non-empty, falling back to `traceparent` otherwise — the same precedence already used when extracting trace context from inbound requests. This setting only affects the router's calls to subgraphs; calls to coprocessors and connectors are unaffected.

By [@OriginLeon](https://github.com/OriginLeon) in https://github.com/apollographql/router/pull/10014
