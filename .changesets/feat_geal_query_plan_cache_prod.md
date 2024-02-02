### Graduate query plan cache from experimental ([Issue #4575](https://github.com/apollographql/router/issues/4575))

The query planner's distributed cache, one of the Router's Enterprise features, has been in use for a long time in production deployments, so it will no longer be marked as experimental.

With this release, we also bring a serie of improvements to this cache:

- replace `.` separator in the Redis cache key with `:`, to align with conventions
- reduce the cache key length
- add the federation version to the cache key, to prevent confusion when routers with different federation versions (and potentially different ways to generate a query plan) target the same cache
- move cache insertion to a parallel task: once the query plan is created, the request can be processed immediately instead of waiting for the cache insertion to finish. This was also fixed for the APQ cache

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4583