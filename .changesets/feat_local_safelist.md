### Support local persisted query manifests for use with offline licenses ([Issue #4587](https://github.com/apollographql/router/issues/4587))

Adds experimental support for passing [persisted query manifests](https://www.apollographql.com/docs/graphos/operations/persisted-queries/#31-generate-persisted-query-manifests) to use instead of the hosted Uplink version. 

For example:

```router.yaml
persisted_queries:
  enabled: true
  log_unknown: true
  experimental_local_manifests: 
    - ./persisted-query-manifest.json
  safelist:
    enabled: true
    require_id: false
```

By [@lleadbet](https://github.com/lleadbet) in https://github.com/apollographql/router/pull/5310
