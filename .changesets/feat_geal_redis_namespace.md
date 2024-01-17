### Redis key namespace ([Issue #4247](https://github.com/apollographql/router/issues/4247))

This implements key namespacing in redis caching. The namespace, if provided, will be added as a prefix to the key: `namespace:key`.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4458