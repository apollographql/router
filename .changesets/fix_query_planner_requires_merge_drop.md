### Fix query planner silently dropping a field's own `@requires` when merged into a same-subgraph ancestor

When a field with its own `@requires` was reached from an ancestor fetch node in the same subgraph, the planner's merge optimization could fold that field directly into the ancestor without checking whether it still had other pending dependencies -- such as a fetch created specifically to satisfy its `@requires`. The dependency was silently discarded: the ancestor's fetch would request the field without first fetching the data it required, producing incomplete or incorrect results with no error or warning.

The query planner's node-merging logic now rejects merging a node into an ancestor unless every other dependency of that node is already satisfied by (i.e., is an ancestor of) the merge target, preserving the correct fetch ordering.

By [@briannafugate408](https://github.com/briannafugate408) in https://github.com/apollographql/router/pull/9967
