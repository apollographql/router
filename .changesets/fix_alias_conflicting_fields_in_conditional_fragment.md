### Alias conflicting subgraph fields inside a conditional fragment ([PR #10377](https://github.com/apollographql/router/pull/10377))

When a subgraph fetch selects two fields with the same response name but incompatible types (for example `f: Int!` on one type and `f: Int` on another, both needed as `@requires` inputs), the query planner aliases one of them and renames it back after the fetch. If those fields were inside an inline fragment that had no type condition, as happens when a `@skip`/`@include` fragment such as `... on Query @include(if: $a)` or `... @include(if: $a)` is turned into a condition in the query plan, the planner recorded the rename but never applied the alias. Query planning then failed with "Query planning produced an invalid subgraph operation ... must not select different types using the same name".

The alias is now applied inside such fragments, so these operations plan normally.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10377
