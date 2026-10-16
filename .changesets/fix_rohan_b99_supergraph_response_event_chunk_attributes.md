### The supergraph response event no longer grows memory use on long-lived streams ([PR #10519](https://github.com/apollographql/router/pull/10519))

With the `telemetry.instrumentation.events.supergraph.response` standard event enabled, the router kept every logged chunk's body of a `@defer` or subscription response for as long as the stream stayed open, and copied all of them each time it logged another chunk. A long-running subscription used more memory and CPU with every event it logged.

The router now keeps only the current chunk's body while it logs that chunk's event. Log output doesn't change: each event still carries its own chunk's `http.response.body`.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/10519
