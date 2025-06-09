### (Query Planner) Fix invalid type condition in `@defer` fetch

The query planner could add an inline spread conditioned on the `Query` type in deferred subgraph fetch queries. Such a query would be invalid in the subgraph when the subgraph schema renamed the root query type. This fix removes the root type condition from all subgraph queries, so that they stay valid even when root types were renamed.

By [@duckki](https://github.com/duckki) in https://github.com/apollographql/router/pull/7580
