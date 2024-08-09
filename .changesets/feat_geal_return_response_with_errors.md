### Entity cache: return cached entities with errors ([PR #5776](https://github.com/apollographql/router/pull/5776))

If we are requesting entities from a subgraph, where some of them are present in cache, and the subgraph is unavailable (ex: network issue), we want to return a response with the entities we got from the cache, other entities nullified, and an error pointing at the paths of unavailable entities.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5776