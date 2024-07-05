### configure introspection caching ([PR #5583](https://github.com/apollographql/router/pull/5583))

Adds an option to deactivate introspection response caching.
Currently, introspection has to go through the query planner, and since that is expensive, the Router caches the introspection responses. This can end up filling the distributed cache, so until we have moved introspection execution entirely out of the planner, we make introspection response caching configurable, as follows:


```yaml
supergraph:
  query_planning:
    legacy_introspection_caching: false
```

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5583