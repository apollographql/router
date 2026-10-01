### Reduce memory use and repeated work during query graph construction ([PR #10358](https://github.com/apollographql/router/pull/10358))

Dense federated query graphs, including connector-expanded schemas, could reserve excessive memory and repeatedly filter identical outgoing edges during graph construction. This increased the cost of schema composition and query planner initialization.

Root-resolution and subgraph-entry transitions now reuse the final filtered followups for each destination. Filtered key followup vectors grow with the surviving edges instead of reserving space for every candidate. Transitions that retain all candidates keep exact-size preallocation, avoiding excess capacity on wide graphs.

Followup edge membership and ordering are preserved, and no configuration changes are required.

By [@cgati](https://github.com/cgati) in https://github.com/apollographql/router/pull/10358
