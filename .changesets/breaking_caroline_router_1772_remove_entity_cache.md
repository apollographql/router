### Remove `preview_entity_cache` plugin

The `apollo.preview_entity_cache` plugin has been removed in Router 3.0. It was superseded by `response_cache`. To migrate, replace your `preview_entity_cache` configuration with `response_cache`:

```yaml
# Before (no longer supported)
preview_entity_cache:
  enabled: true
  subgraph:
    all:
      enabled: true
      redis:
        urls: ["redis://localhost:6379"]
        ttl: 24h

# After
response_cache:
  enabled: true
  subgraph:
    all:
      enabled: true
      redis:
        urls: ["redis://localhost:6379"]
        ttl: 24h
```

**Parity gaps to be aware of when migrating:**

- **`expose_keys_in_context`**: This option exposed raw cache keys into the request context as `apollo::entity_cache::cached_keys_status`. In `response_cache`, enable `debug: true` to expose structured cache debug info (keys, cache control, invalidation details), which is available in the context as `apollo::response_cache::debug_cached_keys`. The data structure is different — see the [surrogate cache key example](https://github.com/apollographql/router/tree/main/examples/coprocessor-surrogate-cache-key) for details.

- **`metrics.separate_per_type`**: The old `apollo.router.operations.entity.cache_hit` metric (a histogram with optional `entity_type` attribute, controlled by `separate_per_type`) has been removed. `response_cache` emits a different set of metrics under `apollo.router.operations.response_cache.*`, which do not include a direct equivalent of the per-entity-type cache hit histogram.

- **`metrics.enabled` / `metrics.ttl`**: `response_cache` metrics are always-on and do not support disabling or configuring a TTL on metric collection.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9689
