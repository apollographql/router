### Fix response cache fetch error metric ([PR #8644](https://github.com/apollographql/router/pull/8644))

The `apollo.router.operations.response_cache.fetch.error` was not being incremented as expected when fetching multiple
items from Redis. This fix changes its behavior to align with the `apollo.router.cache.redis.errors` metric.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/8644
