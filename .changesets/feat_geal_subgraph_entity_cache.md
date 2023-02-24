### Subgraph entity caching ([PR #2526](https://github.com/apollographql/router/pull/2526))

First pass implementation of subgraph entity caching. This will cache individual queries returned by federated queries (not root operations), separated in the cache by type, key, subgraph query, root operation and variables. Data is cached in Redis. This does not support invalidation.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2526
