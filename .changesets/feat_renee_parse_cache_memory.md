### Use a TinyLFU eviction strategy for in-memory caching of GraphQL parsing ([PR #10610](https://github.com/apollographql/router/pull/10610))

The router caches the results of parsing GraphQL queries from a string in memory. Previously, this cache used the LRU algorithm: when the cache was full, any new query would evict the least recently used cache entry.

Now, the router uses the TinyLFU algorithm. This algorithm estimates the frequency of incoming queries, and evicts queries that are not frequently used. This especially results in better cache behaviour if your routers are serving many unique free-form queries: if the cache is full, a unique incoming query may _not_ enter the cache immediately if it would have to evict queries that are frequently used.

The in-memory cache now emits metrics:
- `apollo.platform.cache.memory.size` with `cache.name = query_parsing` reports the number of entries in the cache
- `apollo.platform.cache.lookup.duration` with `cache.name = query_parsing` reports the duration of in-memory cache lookups as a histogram.
  The `cache.status` attribute indicates whether it was a `hit` or a `miss`.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/10610
