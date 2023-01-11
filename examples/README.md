# Examples

This directory contains many examples on how to use and extend the Apollo Router. Each folder may contain `rhai` and/or `rust`, which dictates the kind of plugin it is. For example, [the forbid anonymous operations example](./forbid-anonymous-operations/) has both a Rhai and Rust version available, whereas [the logging example](./logging) is only demonstrated in Rhai. For those without a subfolder, it is intended to be a config-only example, such as the [telemetry samples](./telemetry).

Make sure to look at the docs for:

## Yaml configuration ([documentation](https://www.apollographql.com/docs/router/configuration/overview))

Built in functionality in the Apollo Router.

- [Forbid mutations](./forbid-mutations)
- [Header manipulation](./header-manipulation)
- [Telemetry](./telemetry)
- [Unix sockets](./unix-sockets)

## Customization ([documentation](https://www.apollographql.com/docs/router/customizations/overview))

Extending the functionality of the Apollo Router.

### Rhai

Scripting support.

- [Time request and add to header](./add-timestamp-header/rhai)
- [Cookies to headers](./cookies-to-headers/rhai)
- [Response data modification](./data-response-mutate/rhai)
- [Response errors modification](./error-response-mutate/rhai)
- [Forbid anonymous operations](./forbid-anonymous-operations/rhai)
- [Jwt claims](./jwt-claims/rhai)
- [Logging](./logging/rhai)
- [Operation Name to headers](./op-name-to-header/rhai)
- [Subgraph request logging](./subgraph-request-log/rhai)
- [Surrogate cache key creation](./surrogate-cache-key/rhai)

### Native Rust Plugins

Writing your own plugins in rust!

- [Async auth](./async-auth/rust)
- [Context](./context/rust)
- [Forbid anonymous operations](./forbid-anonymous-operations/rust)
- [Forbid mutations](./forbid-mutations/rust)
- [Hello world](./hello-world/rust)
- [Jwt auth](./jwt-auth/rust)
- [Status code propagation](./status-code-propagation/rust)

### Advanced usage

Customize the router for embedding in a different web server.

- [Embedded](./embedded/rust)
