### Graduate query plan cache from experimental ([Issue #4575](https://github.com/apollographql/router/issues/4575))

The query planner's distributed cache, one of the Router's Enterprise features, has been successfully running in production deployments for a long time. It's now ready to no longer be experimental.

This release also introduces a series of improvements to this cache:
    1. `.` separator is replaced with `:` in the Redis cache key to align with conventions.
    2. Cache key length is reduced.
    3. A federation version is added to the cache key to prevent confusion when routers with different federation versions (and potentially different ways to generate a query plan) target the same cache.
    4. Cache insertion is moved to a parallel task. This means that once the query plan is created, the request can be processed immediately instead of waiting for the cache insertion to finish. This was also fixed for the APQ cache.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4583