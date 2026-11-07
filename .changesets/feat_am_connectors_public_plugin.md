### Promote `connector_request_service` to the public plugin interface ([PR #10049](https://github.com/apollographql/router/pull/10049))

`connector_request_service` is now available on the public `PluginUnstable` trait, so a custom Rust plugin can wrap the service that makes individual connector HTTP requests. It receives the boxed service and a source name, matching the shape of `subgraph_service`. The source name is the same `subgraph_name.source_name` key used for per-source connector configuration (for example, `products.rest`):

```rust
fn connector_request_service(
    &self,
    service: connector::request_service::BoxService,
    source_name: String,
) -> connector::request_service::BoxService {
    // wrap `service` to inspect or rewrite the outgoing request
    service
}
```

The router calls this hook once per connector source when it builds the request pipeline. The service it returns then handles every outbound HTTP request for that source, and one GraphQL operation can produce many of them.

`apollo_router::services::connector::request_service` is public along with what a plugin needs to read and change. Like the hook, everything in it is unstable and may change in any release. The `apollo-federation` types it exposes (`Error` and `RuntimeError`) are re-exported there, so a plugin does not need to depend on `apollo-federation` directly.

On the request:

- `Request::http_request()` / `http_request_mut()` read and rewrite the outgoing `http::Request`: URI, method, headers and body. Both return `None` for a mapping-only connector, which makes no HTTP request.
- `Request::supergraph_request()` returns the router request that produced this connector call, for reading.
- `Request.context` is readable and writable for request-scoped state.
- `Request::into_error_response(message, code, extensions)` fails a connector request without making it, for cases like circuit breaking on an upstream the plugin knows to be unhealthy.

On the response:

- `Response.context` is readable and writable.
- `Response::transport_status()` / `set_transport_status()`, `Response::transport_headers()` / `transport_headers_mut()` and `Response::transport_error()` read and change the raw transport outcome. Each reports nothing to change when there is no HTTP response: a mapping-only connector, or a call that failed at the transport level.
- `Response::data()` / `set_data()` and `Response::error()` / `set_error_message()` / `set_error_code()` read and change what is returned to the client.

Two things are worth knowing when moving a customization between a coprocessor and a plugin:

- The coprocessor `ConnectorRequest` stage cannot change the HTTP method; a plugin can.
- `set_transport_status()` and `transport_headers_mut()` do not recompute the mapped response, so telemetry will report the transport outcome you set while the client receives the unchanged mapped data. Change both, or neither.

The coprocessor `ConnectorRequest` and `ConnectorResponse` stages already covered this, and still do. The difference is that this runs in process, without a round trip per connector request.

By [@andrewmcgivery](https://github.com/andrewmcgivery) in https://github.com/apollographql/router/pull/10049
