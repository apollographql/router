### feat: Introduce PQ Manifest `hot_reload` option for local manifests ([PR #6987](https://github.com/apollographql/router/pull/6987))

This change introduces a `persisted_queries.hot_reload` configuration option in order to allow the router to hot reload local PQ manifest changes.

When using the `local_manifests` option, you can use the `hot_reload` option to tell the router to watch the manifest files for changes and reload them automatically. This is useful if you'd prefer to make updates to the manifest without restarting the router.

```yaml
persisted_queries:
  enabled: true
  local_manifests:
    - ./path/to/persisted-query-manifest.json
  hot_reload: true
```

Note: This change explicitly does _not_ piggyback on the existing `--hot-reload` flag.

By [@trevor-scheer](https://github.com/trevor-scheer) in https://github.com/apollographql/router/pull/6987
