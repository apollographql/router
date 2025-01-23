### Improve performance of query hashing by using a precomputed schema hash ([PR #6622](https://github.com/apollographql/router/pull/6622))

The router now uses a simpler and faster query hashing algorithm with more predictable CPU and memory usage. This improvement is enabled by using a precomputed hash of the entire schema, rather than computing and hashing the subset of types and fields used by each query.
 
For more details on why these design decisions were made, please see the [PR description](https://github.com/apollographql/router/pull/6622)

By [@IvanGoncharov](https://github.com/IvanGoncharov) in https://github.com/apollographql/router/pull/6622