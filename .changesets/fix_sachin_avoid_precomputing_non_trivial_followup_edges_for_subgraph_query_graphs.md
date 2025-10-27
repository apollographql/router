### Prevent unnecessary precomputation during query planner construction ([PR #8373](https://github.com/apollographql/router/pull/8373))

A regression introduced in v2.5.0 caused query planner construction to unnecessarily precompute metadata, leading to increased CPU and memory utilization during supergraph loading. Query planner construction now correctly avoids this unnecessary precomputation.

By [@sachindshinde](https://github.com/sachindshinde) in https://github.com/apollographql/router/pull/8373