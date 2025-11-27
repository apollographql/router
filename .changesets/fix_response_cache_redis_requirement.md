### Validate Redis configuration based on response cache enabled state ([PR #8684](https://github.com/apollographql/router/pull/8684))

Previously, the router would attempt to connect to Redis for response caching regardless of whether response caching was enabled or disabled. This could cause unnecessary connection attempts and configuration errors even when the feature was explicitly disabled.

Now, the router properly validates Redis configuration based on the response cache state:

**When response caching is disabled**: Redis configuration is not required and no connection attempts are made.

**When response caching is enabled**: Redis configuration is validated and required. If a subgraph has caching enabled but no Redis configuration, the router will return a clear error:

```
Error: you must have a redis configured either for all subgraphs or for subgraph "products"
```

This validation ensures that:
- You can disable response caching without needing to provide Redis configuration
- When response caching is enabled, all enabled subgraphs have proper Redis connectivity (either via global `all` configuration or per-subgraph configuration)
- Configuration errors are caught at startup with clear error messages

Example configuration that now works correctly:

```yaml
response_cache:
  enabled: false  # Redis not required when disabled
  # …
  subgraph:
    all:
      # …
      enabled: false
```

```yaml
response_cache:
  enabled: true
  # …
  subgraph:
    all:
      enabled: true
      # …
      redis:  
        urls:
          - redis://127.0.0.1:6379  # Required when enabled
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8684
