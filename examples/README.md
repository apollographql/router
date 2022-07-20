# Examples

This directory contains many examples on how to use and extend the Apollo Router.

Make sure to look at the docs for:

## Yaml configuration ([documentation](https://www.apollographql.com/docs/router/configuration/overview))
Built in functionality in the Apollo Router.

* [Header manipulation](./header-manipulation)
* [Telemetry](./telemetry)
* [Forbid mutations](./forbid_mutations)
* [Unix sockets](./unix-sockets)

## Customization ([documentation](https://www.apollographql.com/docs/router/customizations/overview))
Extending the functionality of the Apollo Router.

### Rhai
Scripting support.
* [Time request and add to header](./add-timestamp-header)
* [Cookies to headers](./cookies-to-headers)
* [Operation Name to headers](./op-name-to-header)
* [Logging](./rhai-logging)
* [Response data modification](./rhai-data-response-mutate)
* [Response errors modification](./rhai-error-response-mutate)
* [Subgraph request logging](./rhai-subgraph-request-log)
* [Surrogate cache key creation](./rhai-surrogate-cache-key)

### Native Rust Plugins
Writing your own plugins in rust!
* [Hello world](./hello-world)
* [Context](./context)
* [Async auth](./async-auth)
* [Jwt auth](./jwt-auth)
* [Forbid mutations](./forbid_mutations)
* [Status code propagation](./status-code-propagation)

### Advanced usage
Customize the router for embedding in a different web server.
* [Embedded](./embedded)

