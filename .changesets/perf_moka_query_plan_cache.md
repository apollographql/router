### perf: migrate in-memory cache from `Mutex<LruCache>` to `moka` (W-TinyLFU) ([PR #TBD](https://github.com/apollographql/router/pull/TBD))

Replaces the `tokio::sync::Mutex<lru::LruCache>` backing `CacheStorage` (used by the query plan, introspection, and APQ caches) with `moka::future::Cache`.

On the hot path (warm-cache hits), concurrent requests now read from moka's sharded concurrent map without acquiring any global lock. The eviction policy changes from LRU to W-TinyLFU, which uses a frequency sketch to protect high-frequency entries from being evicted by cold bursts — particularly relevant for routers serving diverse API consumers where a misbehaving client flooding the cache with inline-constant queries would otherwise silently evict hot plans for every other caller.

No configuration or public API changes.

By [@theJC](https://github.com/theJC) in https://github.com/apollographql/router/pull/TBD
