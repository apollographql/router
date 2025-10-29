### Remove unused TTL parameter from response cache Redis config ([PR #8513](https://github.com/apollographql/router/pull/8513))

The `ttl` parameter was unused; this removes it from the configuration file. TTL configuration should be performed
at the `subgraph` configuration level.

```yaml
preview_response_cache:
  enabled: true
  subgraph:
    all:
      enabled: true
      redis:
        urls: [ "redis://..." ]
      ttl: 10m
```

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/8513