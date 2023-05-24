### hash the query and operation name in the query plan cache key ([Issue #2998](https://github.com/apollographql/router/issues/2998))

The query and operation name can be too large to transmit as part of a Redis cache key. They will now be hashed with SHA256 before writing them as part of the cache key.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3101