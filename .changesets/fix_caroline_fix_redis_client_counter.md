### Correct Redis client counting metric ([PR #8161](https://github.com/apollographql/router/pull/8161))

**The `apollo.router.cache.redis.connections` metric has been removed and replaced with the
`apollo.router.cache.redis.clients` metric.**

The `connections` metric was implemented with an up-down counter which would sometimes not be collected properly (i.e.
it could go negative). The name `connections` was also inaccurate given that the Redis clients will each make multiple
connections, one to each node in the Redis pool (if in clustered mode).

The new `clients` metric counts the number of clients across the router via an `AtomicU64` and surfaces that value in a
gauge.

**Note**: the old metric included a `kind` attribute to reflect the number of clients in each pool (ie entity caching,
query planning). The new metric does not include this attribute; the purpose of the metric is to make sure the number of
clients isn't growing unbounded ([#7319](https://github.com/apollographql/router/pull/7319)).

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/8161
