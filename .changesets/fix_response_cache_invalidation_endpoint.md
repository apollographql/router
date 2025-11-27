### Enable invalidation endpoint for response cache when any subgraph has invalidation enabled ([PR #8680](https://github.com/apollographql/router/pull/8680))

Previously, the response cache invalidation endpoint was only enabled when global invalidation was enabled via `preview_response_cache.subgraph.all.invalidation.enabled`. This meant that if you enabled invalidation for only specific subgraphs without enabling it globally, the invalidation endpoint would not be started, preventing cache invalidation requests from being processed.

Now, the invalidation endpoint is enabled if either:
- Global invalidation is enabled (`preview_response_cache.subgraph.all.invalidation.enabled: true`), OR
- Any individual subgraph has invalidation enabled

This allows for more flexible configuration where you can enable invalidation selectively for specific subgraphs:

```yaml
preview_response_cache:
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
          enabled: true  # Endpoint will now be enabled
          shared_key: ${env.INVALIDATION_SHARED_KEY}
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8680
