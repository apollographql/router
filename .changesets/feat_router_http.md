### Add RouterHttp plugin stage and router_http_service hook ([Issue #6562](https://github.com/apollographql/router/issues/6562))

This pull request introduces a new top-level plugin hook for the Apollo Router at the raw HTTP layer (`RouterHttp`), updates the plugin lifecycle documentation, and adds comprehensive test coverage for the new stage. These changes allow plugins to operate earlier in the request lifecycle, before the Router pipeline, and enable more granular metrics and validation. The license enforcement plugin is also updated to use this new hook.

- **Plugin lifecycle**: Added a new `router_http_service` hook, enabling plugins to run at the RouterHttp stage (raw HTTP layer, before Router pipeline). This is now the earliest point plugins can operate for GraphQL requests.
- **Configuration**: Added `router_http` configuration support and schema definition, allowing request/response configuration at the RouterHttp stage.
- **Tests and metrics**: New metrics and tests for RouterHttpRequest and RouterHttpResponse stages, including helpers and validation for configuration errors.

**Operator notes**

- **License / free-plan limits**: TPS limiting and load shedding run at RouterHttp; overloaded responses still increment `apollo.router.graphql_error` with code `ROUTER_FREE_PLAN_RATE_LIMIT_REACHED` (same as when this logic lived on the Router pipeline).
- **Telemetry**: When not using legacy request spans, the router span is created at RouterHttp so OTLP operation naming stays correct if other `router_http` plugins run first.
- **Static landing**: GET requests that receive the Apollo Sandbox or homepage HTML bypass the RouterHttp plugin stack (see [request lifecycle](https://www.apollographql.com/docs/graphos/routing/request-lifecycle)). Rhai `router_http`, native `router_http_service`, and coprocessor `RouterHttp*` stages are documented to run *before* traffic shaping, limits, and CSRF for normal GraphQL traffic.

By [@smyrick](https://github.com/smyrick) in https://github.com/apollographql/router/pull/8925
