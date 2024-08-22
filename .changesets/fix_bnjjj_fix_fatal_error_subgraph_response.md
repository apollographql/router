### fix(subgraph_service): when the subgraph connection is closed or in error, return a proper subgraph response ([PR #5859](https://github.com/apollographql/router/pull/5859))

When the subgraph connection is closed or in error, return a proper subgraph response containing an error. This was preventing subgraph response service to be triggered in coprocessor and rhai.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5859