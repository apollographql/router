### Reduce allocations and use hash-based node comparison in the query planner ([PR #9803](https://github.com/apollographql/router/pull/9803))

`can_merge_sibling_in` now compares potential merge candidates by node hash instead of a full data comparison; this doesn't change how grandchild-node merges are matched, which a previous commit already handles separately. Several other hot paths in query plan construction avoid needless clones: `subgraph_and_merge_at_key` uses a numeric hash instead of string formatting, `possible_types` iterates instead of cloning, `Copy`-type `NodeIndex` values are copied instead of cloned, and `flat_wrap_nodes` moves owned `Vec<PlanNode>` values instead of cloning them.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/9803
