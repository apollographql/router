### Entity cache returns cached entities with errors ([PR #5776](https://github.com/apollographql/router/pull/5776))

When requesting entities from a subgraph where some entities are cached but the subgraph is unavailable (for example, due to a network issue), the router now returns a response with the cached entities retrieved, the unavailable entities nullified, and an error pointing at the paths of the unavailable entities.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5776