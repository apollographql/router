### Graduate distributed query plan caching from experimental ([Issue #4575](https://github.com/apollographql/router/issues/4575))

[Distributed query plan caching] (https://www.apollographql.com/docs/router/configuration/distributed-caching#distributed-query-plan-caching) has been validated in production deployments and is now a fully supported, non-experimental Enterprise feature of the Apollo Router.

To migrate your router configuration, replace `supergraph.query_planning.experimental_cache` with `supergraph.query_planning.cache`.

This release also adds improvements to the distributed cache:
    1. The `.` separator is replaced with `:` in the Redis cache key to align with conventions.
    2. The cache key length is reduced.
    3. A federation version is added to the cache key to prevent confusion when routers with different federation versions (and potentially different ways to generate a query plan) target the same cache.
    4. Cache insertion is moved to a parallel task. Once the query plan is created, this allows a request to be processed immediately instead of waiting for cache insertion to finish. This improvement has also been applied to the APQ cache.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4583