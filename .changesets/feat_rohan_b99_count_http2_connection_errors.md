### Count HTTP/2 connections the router closes with an error

The router now records `apollo.router.http2.server.connection.errors`, a counter of client HTTP/2 connections it closed with an HTTP/2 error code. The `error.type` attribute holds the error code, such as `ENHANCE_YOUR_CALM` or `PROTOCOL_ERROR`. Connections the client closes, network errors and graceful shutdowns are not counted.

When a request's header list is far over `limits.router.http2_max_headers_list_bytes`, the router closes the connection without an HTTP response, so the request never appears in `http.server.request.duration` or traces. These closes are now counted with `error.type` set to `ENHANCE_YOUR_CALM`. The same code also covers other abuse protections, such as CONTINUATION-frame floods, and the router can't tell them apart. See [Standard instruments](https://www.apollographql.com/docs/graphos/routing/observability/router-telemetry-otel/enabling-telemetry/standard-instruments#server) for every `error.type` value.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/10480
