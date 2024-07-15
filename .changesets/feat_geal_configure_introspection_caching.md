### Add option to deactivate introspection response caching ([PR #5583](https://github.com/apollographql/router/pull/5583))

The router now supports an option to deactivate introspection response caching. Because the router caches responses as introspection happens in the query planner, cached introspection responses may consume too much of the distributed cache or fill it up. Setting this option prevents introspection responses from filling up the router's distributed cache.

To deactivate introspection caching, set `supergraph.query_planning.legacy_introspection_caching` to `false`:


```yaml
supergraph:
  query_planning:
    legacy_introspection_caching: false
```

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5583