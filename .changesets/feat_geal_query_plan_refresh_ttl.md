### Add an option to refresh expiration on Redis GET ([Issue #4473](https://github.com/apollographql/router/issues/4473))

This adds the option to refresh the TTL on Redis entries when they are accessed. We want the query plan cache to act like a LRU, so if a TTL is set in its Redis configuration, it should reset every time it is accessed.

The option is also available for APQ, but it is disabled for entity caching, since that cache directly manages TTL.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4604