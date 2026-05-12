### Avoid unnecessary clones on subgraph requests ([PR #9266](https://github.com/apollographql/router/pull/9266))

The router now avoids some unnecessary memory allocations when making subgraph requests, particularly on the APQ (Automatic Persisted Queries) path.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9266
