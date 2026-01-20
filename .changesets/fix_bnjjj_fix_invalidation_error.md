### Response cache: returns error when you have an error invalidating data ([PR #8813](https://github.com/apollographql/router/pull/8813))

Without this fix it will never throw an error. Once you deploy this change you might see some increase in `apollo.router.operations.response_cache.invalidation.error` metric.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8813