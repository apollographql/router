### Enable invalidation endpoint when any subgraph has invalidation enabled ([PR #8680](https://github.com/apollographql/router/pull/8680))

Previously, the response cache invalidation endpoint was only enabled when global invalidation was enabled via `response_cache.subgraph.all.invalidation.enabled`. If you enabled invalidation for only specific subgraphs without enabling it globally, the invalidation endpoint wouldn't start, preventing cache invalidation requests from being processed.

The invalidation endpoint now starts if either:
- Global invalidation is enabled (`response_cache.subgraph.all.invalidation.enabled: true`), OR
- Any individual subgraph has invalidation enabled

This enables more flexible configuration where you can enable invalidation selectively for specific subgraphs:

```yaml
response_cache:
  enabled: true
  invalidation:
    listen: 127.0.0.1:4000
    path: /invalidation
  subgraph:
    all:
      enabled: true
      # Global invalidation not enabled
    subgraphs:
      products:
        invalidation:
          enabled: true  # Endpoint now starts
          shared_key: ${env.INVALIDATION_SHARED_KEY}
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8680
