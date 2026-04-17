### Emit `graphql.error.extensions.code` on span events for all counted GraphQL errors ([PR #9207](https://github.com/apollographql/router/pull/9207))

The `apollo.router.operations.error` metric carries `graphql.error.extensions.code` for every counted GraphQL error, but the matching span event only fired for errors raised by the `demand_control` and `connectors` plugins. Subgraph-returned, supergraph, execution, and router parse/validation errors reached OTLP traces without the code attribute, so trace-based consumers could not attribute errors to specific codes the way metric-based consumers already could.

The router now emits the span event for all counted errors, gated on the same flag as the metric (`telemetry.apollo.errors.preview_extended_error_metrics: enabled`). Users who have not opted in see no behavior change.

By [@david-castaneda](https://github.com/david-castaneda) in https://github.com/apollographql/router/pull/9207
