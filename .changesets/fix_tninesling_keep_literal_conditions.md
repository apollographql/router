### Keep literal `@skip`/`@include` in subgraph fetches under a variable condition ([PR #10708](https://github.com/apollographql/router/pull/10708))

When a field had a variable `@skip` or `@include`, the query planner dropped `@include(if: false)` and `@skip(if: true)` from nested fields in the same subgraph fetch. Subgraphs then resolved fields the client excluded. The router still removed them from the response, so the cost was extra subgraph work. These directives are now sent to the subgraph.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/10708
