### Use supergraph schema to extract authorization info ([PR #5047](https://github.com/apollographql/router/pull/5047))

The router now uses the supergraph schema to extract authorization info, as authorization information may not be available on the query planner's subgraph schemas. This reverts the authorization changes made in [PR #4975](https://github.com/apollographql/router/pull/4975).

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/5047
