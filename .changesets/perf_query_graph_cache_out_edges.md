### Cache sorted out-edges during followup precompute ([PR #9690](https://github.com/apollographql/router/pull/9690))

During federated query graph construction, `precompute_non_trivial_followup_edges()` called `out_edges()` once per edge, and each of those calls filtered and sorted a fresh list of the tail node's outgoing edges. On connector-heavy supergraphs, where many synthetic subgraphs share entity types and produce a dense cross-subgraph key graph, the same lists were rebuilt over and over.

The filtered and sorted list is now computed once per tail node and reused. The edge set and its order are unchanged, so query plans are unaffected. On a connector-expanded supergraph with 32 connectors, total allocation during startup drops from 5.85 GB to 0.85 GB and startup time drops from 40.8s to 31.7s.

By [@benjamn](https://github.com/benjamn) in <https://github.com/apollographql/router/pull/9690>
