### Preview support for GraphQL Federation source schemas ([PR #TBD](https://github.com/apollographql/router/pull/TBD))

The router can now serve supergraphs that include [GraphQL Federation](https://github.com/graphql/composite-schemas-spec) source schemas (the specification formerly known as Composite Schemas). A source schema doesn't implement `_entities`. It exposes entities through `@lookup` fields, whose arguments are mapped to key fields with `@is`. It can also take data from other subgraphs through `@require` arguments. Source schemas and Apollo Federation subgraphs can be composed into the same supergraph.

This is a preview. Composition emits `join` v0.6 only for supergraphs that include a source schema, and the router refuses those supergraphs unless `preview_graphql_federation` is enabled. Enabling it also requires the incremental query planner:

```yaml
supergraph:
  query_planning:
    incremental_planner:
      enabled: true
preview_graphql_federation:
  enabled: true
  subgraph:
    all:
      # Send all the entities of a lookup fetch in one request, as a list of variable sets.
      variable_batching: true
      # Combine lookup fetches to the same subgraph that run in parallel into one HTTP request.
      request_batching: true
      # Split batches larger than this into several requests.
      maximum_size: 100
    subgraphs:
      products:
        variable_batching: false
```

Without batching, the router sends one request per entity, calling the lookup field with that entity's key. Batching follows the draft "Batching" appendix of the GraphQL-over-HTTP specification, and the subgraph must support it. Batched responses are read as `application/jsonl`, one result per line, placed by `variableIndex` and `requestIndex`. A JSON array answer to a request batch is also accepted.

The new `apollo.router.operations.graphql_federation.lookup_batches` counter records batched lookup requests. The `apollo.router.config.graphql_federation` gauge reports whether the feature is enabled.

By [@dariuszkuc](https://github.com/dariuszkuc) in https://github.com/apollographql/router/pull/TBD
