### Use supergraph schema to extract auth info ([PR #5047](https://github.com/apollographql/router/pull/5047))

Use supergraph schema to extract auth info as auth information may not be available on the query planner's subgraph schemas. This undoes the auth changes made in #4975.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/5047
