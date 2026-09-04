# Connector request header

Demonstrates `PluginUnstable::connector_request_service` via a native Rust plugin that wraps the
service making individual HTTP requests to Apollo Connectors, adding a header to every outbound
request.

This is the same reach as the coprocessor `ConnectorRequest`/`ConnectorResponse` stages, but
in-process, without a round trip per connector request. See
[`Request`](https://docs.rs/apollo-router/latest/apollo_router/services/connector/request_service/struct.Request.html)
and
[`Response`](https://docs.rs/apollo-router/latest/apollo_router/services/connector/request_service/struct.Response.html)
for the full set of what a plugin can read and change on each side.

## Usage

Running this against real traffic requires a supergraph with at least one `@connect`-annotated
subgraph. Once you have one:

```bash
cargo run -- -s <path-to-a-connectors-supergraph.graphql> -c ./router.yaml
```

## Implementation

`connector_request_service` is defined on `PluginUnstable` rather than the stable `Plugin` trait,
so it may still change shape -- implementing `PluginUnstable` requires the extra
`unstable_method` with no default body, making that opt-in visible in the plugin.
