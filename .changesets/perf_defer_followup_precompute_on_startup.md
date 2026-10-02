### Defer followup-edge precompute on the query-planning path ([PR #9691](https://github.com/apollographql/router/pull/9691))

Building the federated query graph eagerly computed, for every edge in the graph, the list of non-trivial followup edges. On connector-heavy supergraphs the graph contains many cross-subgraph key edges, and that precompute dominated both peak heap and startup time.

The router's query-planning graph build now skips the eager precompute and falls back to ordinary out-edge traversal during planning. The precomputed map is only a pruning optimization over that traversal, so query plans are unchanged. Composition satisfiability checking still precomputes as before.

On a connector-expanded supergraph with 64 connectors, the full stack of this change and PR #9690 reduces peak heap from 2.13 GB to 367 MB, total allocation from 40.4 GB to 1.27 GB, and startup time from 129s to 52s.

By [@benjamn](https://github.com/benjamn) in <https://github.com/apollographql/router/pull/9691>
