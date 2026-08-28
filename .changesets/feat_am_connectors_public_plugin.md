### Promote `connector_request_service` to the public plugin interface ([PR #10049](https://github.com/apollographql/router/pull/10049))

`connector_request_service` is now available on the public `PluginUnstable` trait, so a custom Rust plugin can wrap the service that makes individual connector HTTP requests. It receives the boxed service and the `@source` name, matching the shape of `subgraph_service`:

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

`apollo_router::services::connector::request_service` is public along with what a plugin needs to read and change. One GraphQL operation can produce many connector requests, so this service runs once per outbound HTTP request rather than once per operation.

On the request:

- `Request.transport_request` carries the underlying `http::Request`, so a plugin can read and rewrite the URI, method, headers and body.
- `Request.supergraph_request()` returns the router request that produced this connector call, for reading.
- `Request.context` is readable and writable for request-scoped state.
- `Request::into_error_response(message, code)` fails a connector request without making it, for cases like circuit breaking on an upstream the plugin knows to be unhealthy.

On the response:

- `Response.context` is readable and writable.
- `Response.transport_result` exposes the raw transport outcome: status, headers, and transport-level errors.
- `Response::data()` / `set_data()` and `Response::error()` / `set_error_message()` / `set_error_code()` read and change what is returned to the client.

Two things are worth knowing when moving a customization between a coprocessor and a plugin:

- The coprocessor `ConnectorRequest` stage cannot change the HTTP method; a plugin can.
- Changing `Response.transport_result` does not recompute the mapped response, so telemetry will report the transport outcome you set while the client receives the unchanged mapped data. Change both, or neither.

The coprocessor `ConnectorRequest` and `ConnectorResponse` stages already covered this, and still do. The difference is that this runs in process, without a round trip per connector request.

By [@andrewmcgivery](https://github.com/andrewmcgivery) in https://github.com/apollographql/router/pull/10049
