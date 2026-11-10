### Build no spans where nothing reads them

The router built every span of every request, through the subscriber's registry and the OpenTelemetry layer, whether or not anything read it. It now disables spans at their callsites when nothing in its configuration reads them: no trace exporter (OTLP, Datadog, or Apollo's with an Apollo key and graph reference), no propagator, no response trace id, no instrument, event or span attribute reading the trace id or baggage, no configured event, no span-scoped log directive, and a stdout format that prints no span data. Span fields are also recorded only while the stdout format prints them.

The default stdout formats print span data (the JSON format its span list, trace id and span id, the text format its span list and current span), so a router at its defaults keeps building spans and saves nothing, and so does any router with an Apollo key and graph reference or a trace exporter. To build no spans when nothing else reads them, turn those settings off, for example:

```yaml
telemetry:
  exporters:
    logging:
      stdout:
        format:
          json:
            display_span_list: false
            display_trace_id: false
            display_span_id: false
```

Subgraphs keep receiving the `apollo-federation-include-trace` header at the field-level instrumentation rate when no span is built. Where no span is built, the public `apollo_router::tracer::TraceId::maybe_new` returns `None`.

By [@martijnwalraven](https://github.com/martijnwalraven)
