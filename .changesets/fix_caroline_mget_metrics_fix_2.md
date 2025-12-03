### Fix response cache fetch error metric ([PR #8711](https://github.com/apollographql/router/pull/8711))

The `apollo.router.operations.response_cache.fetch.error` metric was out of sync with the `apollo.router.cache.redis.errors` metric, because errors were not being returned from the Redis client wrapper. This changes the response caching plugin to increment the error metric as expected.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/8711