### Reuse response cache Redis connections for identical subgraph configuration ([PR #8764](https://github.com/apollographql/router/pull/8764))

The response cache now reuses Redis connection pools when subgraph-level configuration resolves to the same Redis configuration as the global `all` setting. Previously, the router could create redundant Redis connections even when the effective configuration was identical.

Impact: If you configure response caching at both the global and subgraph levels, you should see fewer Redis connections and lower connection overhead.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8764