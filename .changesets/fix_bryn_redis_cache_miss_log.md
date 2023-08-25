### Error no longer reported on Redis cache misses ([Issue #2876](https://github.com/apollographql/router/issues/2876))

The Router will no longer log an error in when fetching from Redis and the record doesn't exist. This affected APQ, QueryPlanning and experimental entity caching.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/3661
