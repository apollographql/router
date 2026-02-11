### Add HTTP service support for coprocessors and Rhai plugins ([Issue #6562](https://github.com/apollographql/router/issues/6562))

Coprocessors and Rhai plugins can now use external HTTP services instead of running inline. This lets you implement coprocessor logic in any language (e.g. Node.js, Python) or run it as a separate process.

**Coprocessor HTTP service:**
- Configure a coprocessor via `coprocessor.url` pointing to an HTTP endpoint
- New `router_http` stage runs at the raw HTTP layer, before GraphQL parsing—useful for reading/mutating request/response headers and body as bytes
- New `service_http` stage for outbound HTTP to subgraphs and connectors
- Payload shape matches the router request/response externalization format

**Rhai HTTP service:**
- New `register_http_service()` function in Rhai lets plugins delegate work to external HTTP services
- Useful for sharing logic across Rhai and native plugins or calling out to existing microservices

**Examples:**
- `examples/coprocessor-http-service/nodejs/` – Node.js coprocessor demonstrating the router_http stage
- `examples/rhai-http-service/` – Rhai plugin calling an HTTP service

By [@smyrick](https://github.com/smyrick) in https://github.com/apollographql/router/pull/8874
