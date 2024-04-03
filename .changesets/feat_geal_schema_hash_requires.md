### reuse cached query plans across schema updates if possible ([Issue #4834](https://github.com/apollographql/router/issues/4834))

This extends the schema aware query hashing introduced in entity caching, to reduce the amount of work when reloading the router. That hash is designed to stay the same for a same query across schema updates if the update does not affect that query. If query planner cache warm up is configured, then we will reuse previous cache entries for which the hash does not change, which will reduce CPU usage and make reloads faster.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4883