### Remove `preview_entity_cache` plugin

The `apollo.preview_entity_cache` plugin has been removed in Router 3.0. It was superseded by `response_cache`, which provides the same caching functionality. To migrate, replace your `preview_entity_cache` configuration with `response_cache`:

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

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/XXXX
