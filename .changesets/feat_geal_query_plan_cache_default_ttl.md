### set a default TTL for query plans ([Issue #4473](https://github.com/apollographql/router/issues/4473))

This sets a default TTL of 30 days for query plan caches, because the previous default was to store query plans indefinitely, which does not make sense because they change with schema updates.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4588