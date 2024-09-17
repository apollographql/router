### Enable query fragment generation by default

The router previously had `experimental_reuse_query_fragments` enabled by
default when trying to optimize fragments before sending operations to subgraphs.
While on occasion this algorithm can be more performant, we found that in vast
majority of cases the query planner can be just as performant using
`generate_query_fragments` query plan option, which also significantly reduces
query size being sent to subgraphs. While the two options will produce correct
responses, the queries produced internally by the query planner will differ.

This change enables `generate_query_fragments` by default, while disabling
`experimental_reuse_query_fragments`. You can change this behavior with the
following options:

```yaml
supergraph:
  generate_query_fragments: false
  experimental_reuse_query_fragments: true
```

By [@lrlna](https://github.com/lrlna) in https://github.com/apollographql/router/pull/6013