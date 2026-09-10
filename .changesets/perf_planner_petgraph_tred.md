### Replace DFS transitive reduction with petgraph algorithm ([PR #9804](https://github.com/apollographql/router/pull/9804))

The query planner's `reduce()` step previously used a hand-written DFS to compute the transitive reduction of the fetch dependency graph. This has been replaced with petgraph's `dag_transitive_reduction_closure`, which implements the Habib–Morvan–Rampon algorithm in O(V+E) time.

By [@tninesling](https://github.com/tninesling) in <https://github.com/apollographql/router/pull/9804>
