### Evaluate `on_graphql_error` per response part in coprocessors and telemetry for `@defer` responses ([PR #9365](https://github.com/apollographql/router/pull/9365))

Previously, the `on_graphql_error` condition used in coprocessor and telemetry configurations did not work correctly for deferred (`@defer`) multipart responses:

- The **supergraph coprocessor** evaluated the condition once before any stream chunks were consumed, so it never fired for errors that appeared only in incremental chunks.
- The **router coprocessor** read the sticky `CONTAINS_GRAPHQL_ERROR` context flag, which caused it to fire for every subsequent chunk once any earlier chunk had errors — and to never fire if the first chunk was clean.
- The `on_graphql_error` **telemetry selector** at the supergraph stage returned the accumulated error state rather than the current chunk's error state.

The router and supergraph coprocessors, along with telemetry selectors, now evaluate `on_graphql_error` conditions per response part, so the condition fires exactly once per part that contains GraphQL errors — no more, no less.

Additionally, `on_graphql_error: false` (fire when there are *no* errors) now works correctly in all selector contexts: router, supergraph, and subgraph.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9365
