### Reuse generated fragments for equivalent selections that list same-named fields in a different order ([PR #10375](https://github.com/apollographql/router/pull/10375))

When `supergraph.generate_query_fragments` is enabled, the query planner extracts a named fragment for every sub-selection that appears more than once in a subgraph operation. Two sub-selections that contained the same field more than once under different directives, such as `x @include(if: $v)` and `x @skip(if: $v)`, were treated as different selections when those fields appeared in a different order, so no fragment was shared between them. Such sub-selections are now recognized as equivalent, and the generated subgraph operation reuses one fragment for both.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10375
