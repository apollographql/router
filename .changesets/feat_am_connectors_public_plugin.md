### Promote `connector_request_service` to the public plugin interface ([PR #10049](https://github.com/apollographql/router/pull/10049))

`connector_request_service` is now available on the public `Plugin` and `PluginUnstable` traits, so a custom Rust plugin can wrap the service that makes individual connector HTTP requests. It receives the boxed service and the `@source` name, matching the shape of `subgraph_service`:

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

`apollo_router::services::connector::request_service` is public along with the fields a plugin needs: `Request.context`, `Request.transport_request`, `Request.supergraph_request`, and `Response.transport_result`. Because `TransportRequest` carries the underlying `http::Request`, a plugin can read and rewrite the URI, method, headers, and body per connector request.

The coprocessor `ConnectorRequest` stage already covered this, and still does. The difference is that this runs in process, without a round trip per connector request.

By [@andrewmcgivery](https://github.com/andrewmcgivery) in https://github.com/apollographql/router/pull/10049
