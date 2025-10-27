### Query planning errors with progressive override on interface implementations ([PR #7929](https://github.com/apollographql/router/pull/7929))

The router now correctly generates query plans when using [progressive override](https://www.apollographql.com/docs/graphos/schema-design/federated-schemas/entities/migrate-fields#incremental-migration-with-progressive-override) (`@override` with labels) on types that implement interfaces within the same subgraph.

Previously, the Rust query planner would fail to generate plans for these scenarios with the error `"Was not able to find any options for {}: This shouldn't have happened."`, while the JavaScript planner handled them correctly.

This fix resolves planning failures when your schema uses:

- Interface implementations local to a subgraph
- Progressive override directives on both the implementing type and its fields
- Queries that traverse through the overridden interface implementations

These will now successfully plan and execute.

By [@TylerBloom](https://github.com/TylerBloom) in https://github.com/apollographql/router/pull/7929
