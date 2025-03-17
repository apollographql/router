### feat: Introduce PQ Manifest `hot_reload` option for local manifests ([PR #6987](https://github.com/apollographql/router/pull/6987))

This change introduces a [`persisted_queries.hot_reload` configuration option](https://www.apollographql.com/docs/graphos/routing/security/persisted-queries#hot_reload) to allow the router to hot reload local PQ manifest changes.

If you configure `local_manifests`, you can set `hot_reload` to `true` to automatically reload manifest files whenever they change. This lets you update local manifest files without restarting the router.

```yaml
persisted_queries:
  enabled: true
  local_manifests:
    - ./path/to/persisted-query-manifest.json
  hot_reload: true
```

Note: This change explicitly does _not_ piggyback on the existing `--hot-reload` flag.

By [@trevor-scheer](https://github.com/trevor-scheer) in https://github.com/apollographql/router/pull/6987
