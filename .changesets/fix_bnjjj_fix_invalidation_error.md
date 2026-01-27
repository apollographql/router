### Report response cache invalidation failures as errors ([PR #8813](https://github.com/apollographql/router/pull/8813))

The router now returns an error when response cache invalidation fails. Previously, an invalidation attempt could fail without being surfaced as an error.

After you upgrade, you might see an increase in the `apollo.router.operations.response_cache.invalidation.error` metric.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8813