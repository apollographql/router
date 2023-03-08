### Use the `fred` Redis client ([Issue #2623](https://github.com/apollographql/router/issues/2623))

Use the `fred` Redis client instead of the `redis` and `redis-cluster-async` crates. This simplifies the code, adds support for TLS in cluster mode, and removes OpenSSL usage.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2689