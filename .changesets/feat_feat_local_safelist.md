### Allow use of a local persisted query manifest for use with offline licenses ([Issue #4587](https://github.com/apollographql/router/issues/4587))

This adds support to be able to pass a [Persisted Query](https://www.apollographql.com/docs/graphos/operations/persisted-queries/) safelist/manifest to be used in place of the hosted Uplink version. 

An example configuration would look like:

```yml
persisted_queries:
  enabled: true
  log_unknown: true
  safelist:
    enabled: true
    require_id: false
    local_safelist: ./persisted-query-manifest.json
```

By [@lleadbet](https://github.com/lleadbet) in https://github.com/apollographql/router/pull/5310
