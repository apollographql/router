### Interrupted subgraph connections trigger error responses and subgraph service hook points ([PR #5859](https://github.com/apollographql/router/pull/5859))

The router now returns a proper subgraph response, with an error if necessary, when a subgraph connection is closed or returns an error.

Previously, this issue prevented the subgraph response service from being triggered in coprocessors or Rhai scripts.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5859