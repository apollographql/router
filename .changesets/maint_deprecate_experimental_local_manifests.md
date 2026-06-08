### Deprecate `persisted_queries.experimental_local_manifests`

The `persisted_queries.experimental_local_manifests` configuration key is now deprecated. Operators using this key will see a deprecation warning at router startup directing them to the GA `persisted_queries.local_manifests` key, which has the same behavior. The deprecated key continues to work in 2.x via the existing config migration, but will be removed in 3.x.

```yaml
# Before
persisted_queries:
  enabled: true
  experimental_local_manifests:
    - ./manifest.json

# After
persisted_queries:
  enabled: true
  local_manifests:
    - ./manifest.json
```

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/9523
