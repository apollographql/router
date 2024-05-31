### Allow use of a local persisted query manifest for use with offline licenses ([Issue #4587](https://github.com/apollographql/router/issues/4587))

This adds support to be able to pass a [Persisted Query](https://www.apollographql.com/docs/graphos/operations/persisted-queries/) manifest to be used in place of the hosted Uplink version. 

An example configuration would look like:

```yml
persisted_queries:
  enabled: true
  log_unknown: true
  local_manifest: ./persisted-query-manifest.json
  safelist:
    enabled: true
    require_id: false
```

By [@lleadbet](https://github.com/lleadbet) in https://github.com/apollographql/router/pull/5310
