### Set a default TTL for query plans ([Issue #4473](https://github.com/apollographql/router/issues/4473))

The router has updated the default TTL for query plan caches. The new default TTL is 30 days. With the previous default being an infinite duration, the new finite default better supports the fact that the router updates caches with schema updates.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4588