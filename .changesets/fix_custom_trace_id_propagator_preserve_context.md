### Preserve valid W3C trace context when the custom trace ID header is absent ([PR #9984](https://github.com/apollographql/router/pull/9984))

When `telemetry.exporters.tracing.propagation.request.header_name` is
configured, the router registers `CustomTraceIdPropagator` as the last
propagator in the chain so it can override earlier ones when its own header
is present. Previously, when that header was absent on a given request, it
unconditionally reset the trace context to an empty one, discarding a valid
context an earlier propagator (e.g. W3C `trace_context`) had already
extracted from an incoming `traceparent` header — forcing a new root trace
on every such request.

The propagator now leaves the context untouched when its header is missing,
so a context extracted by an earlier propagator is preserved.

By [@OriginLeon](https://github.com/OriginLeon) in https://github.com/apollographql/router/pull/9984
