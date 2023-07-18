### Update router bridge and add option for `reuse_query_fragments`  ([Issue #3452](https://github.com/apollographql/router/issues/3452))

Federation v2.4.9 enabled a new feature for [query fragment reuse](https://github.com/apollographql/federation/pull/2639) that is causing issues for some users.

A new option has been added to he router config file to opt into this feature:
```yaml
supergraph:
  reuse_query_fragments: true
```

The default is disabled.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/3453
