### The incremental query planner is now the default ([PR #10331](https://github.com/apollographql/router/pull/10331))

`supergraph.query_planning.incremental_planner.enabled` now defaults to `true`. Subgraph operations may list fields in a different order, omit `__typename` or key fields the operation does not need, and fetch `@requires` inputs under an alias.

The incremental planner does not support the following. Set `enabled: false` to keep the previous planner if you rely on them:

- `experimental_type_conditioned_fetching`, which the incremental planner ignores.
- `experimental_plans_limit` and `experimental_paths_limit`, which only apply to the previous planner.
- Connectors that resolve entity types without an `@key` in the connector's subgraph (connectors on types, field connectors using `$this`, and `entity: true` connectors). These fail to plan.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/10331
