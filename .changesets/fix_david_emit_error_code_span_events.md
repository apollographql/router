### Emit `graphql.error.extensions.code` on span events for all counted GraphQL errors ([PR #9207](https://github.com/apollographql/router/pull/9207))

The `apollo.router.operations.error` metric carries `graphql.error.extensions.code` for every counted GraphQL error, but the matching span event only fired for errors raised by the `demand_control` and `connectors` plugins. Subgraph-returned, supergraph, execution, and router parse/validation errors reached OTLP traces without the code attribute, so trace-based consumers could not attribute errors to specific codes the way metric-based consumers already could.

The router now also emits the span event from `count_operation_errors` as a catch-all, gated on the same flag as the metric (`telemetry.apollo.errors.preview_extended_error_metrics: enabled`). The `connectors` and `demand_control` plugins continue to emit on their own spans so the event keeps the source-site attributes (connector coordinate, demand control context, etc.); to avoid double-emission, `graphql::Error` carries a non-serialized `span_event_emitted` flag that the catch-all checks and respects. The metric still increments either way, and the flag is never serialized into the user-facing error response.

By [@david-castaneda](https://github.com/david-castaneda) in https://github.com/apollographql/router/pull/9207
