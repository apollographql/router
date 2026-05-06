### Clarify authorization directive behavior on federated root operation types

The authorization docs now explain what happens when you apply `@authenticated`, `@requiresScopes`, or `@policy` directly to a root operation type (`Query`, `Mutation`, or `Subscription`) in a subgraph. Because root operation types are shared merged types in a federated graph, the directive composes into the supergraph root type and applies to every field on that type, including fields contributed by other subgraphs. To scope authorization reliably, apply the directive to each field rather than to the root type.

By [@andywgarcia](https://github.com/andywgarcia) in https://github.com/apollographql/router/pull/9213
