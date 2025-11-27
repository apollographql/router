### Remove unused TTL parameter from response cache Redis configuration ([PR #8513](https://github.com/apollographql/router/pull/8513))

The `ttl` parameter under `redis` configuration had no effect and is removed. Configure TTL at the `subgraph` level to control cache entry expiration:

```yaml
preview_response_cache:
  enabled: true
  subgraph:
    all:
      enabled: true
      ttl: 10m  # ✅ Configure TTL here
      redis:
        urls: [ "redis://..." ]
        # ❌ ttl was here previously (unused)
```

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/8513