### support TTL in redis storage ([Issue #4163](https://github.com/apollographql/router/issues/4163))

It is now possible to set an expiration for distributed caching entries, both for APQ and query planning caches, using the configuration file:

```yaml title="router.yaml"
supergraph:
  query_planning:
    experimental_cache:
      redis:
        urls: ["redis://..."]
        timeout: 5ms # Optional, by default: 2ms
        ttl: 24h # Optional, by default no expiration
```

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4164