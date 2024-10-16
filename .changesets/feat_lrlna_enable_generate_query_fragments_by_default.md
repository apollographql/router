### Compress subgraph operations by generating fragments

The router now compresses operations sent to subgraphs by default by generating fragment
definitions and using them in the operation.

Initially, the router is using a very simple transformation that is implemented in both
the JavaScript and Native query planners. We will improve the algorithm after the JavaScript
planner is no longer supported.

This replaces a previous experimental algorithm that was enabled by default.
`experimental_reuse_query_fragments` attempted to intelligently reuse the fragment definitions
from the original operation. Fragment generation is much faster, and in most cases produces
better outputs too.

If you are relying on the shape of fragments in your subgraph operations or tests, you can opt
out of the new algorithm with the configuration below. Note we strongly recommend against
relying on the shape of planned operations as new router features and optimizations may affect
it, and we intend to remove `experimental_reuse_query_fragments` in a future release.

```yaml
supergraph:
  generate_query_fragments: false
  experimental_reuse_query_fragments: true
```

By [@lrlna](https://github.com/lrlna) in https://github.com/apollographql/router/pull/6013
