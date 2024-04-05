### Add `generate_query_fragments` configuration option ([PR #4885](https://github.com/apollographql/router/pull/4885))

Add a new `supergraph` configuration option `generate_query_fragments`. When set to `true`, the query planner will extract inline fragments into fragment definitions before sending queries to subgraphs. This can significantly reduce the size of the query sent to subgraphs, but may increase the time it takes to plan the query. Note that this option and `reuse_query_fragments` are mutually exclusive; if both are set to `true`, `generate_query_fragments` will take precedence.

An example router configuration:

```yaml title="router.yaml"
supergraph:
  generate_query_fragments: true
```

By [@trevor-scheer](https://github.com/trevor-scheer) in https://github.com/apollographql/router/pull/4885
