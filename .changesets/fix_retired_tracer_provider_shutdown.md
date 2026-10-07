### Shut down the replaced tracer provider off async worker threads during reload ([PR #10276](https://github.com/apollographql/router/pull/10276))

When a schema or configuration reload replaced the tracing configuration, the previous OpenTelemetry tracer provider was shut down by whichever thread released its last reference. Spans hold a reference to their provider, so a span being exported at the exact moment of the swap could run that shutdown on an async worker thread. The batch span processor's shutdown blocks that thread while waiting for a background task that only the same thread could run, so the worker could stall permanently. Everything that worker was holding, including the retired request pipeline with its caches and Redis connections, then stayed in memory until the router was restarted, while the remaining workers kept serving traffic.

The router now shuts down the replaced tracer provider explicitly on a blocking thread straight after the swap. A span that still holds the old provider no longer triggers shutdown when it is dropped. Spans that finish on the old provider after the swap are discarded rather than exported.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/10276
