### Connectors can map streamed responses with `http.stream: "array"` (connect/v0.5)

A connector that sets `stream: "array"` in its `http` argument can now map Server-Sent Events and JSON Lines responses: its selection receives the list of JSON events, in order. A plain JSON body becomes a list of one, so a single selection such as `$->last.result` handles an MCP `tools/call` whether the server answers with SSE or JSON. Such connectors also send an `Accept` header covering all three content types, unless they set their own.

Connectors without `stream` are unchanged. The router reports adoption with the `apollo.router.schema.connectors.stream` gauge.

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/10658
