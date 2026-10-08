### Further reduce memory and time of query graph construction for connector-heavy supergraphs ([PR #10346](https://github.com/apollographql/router/pull/10346))

Every Apollo Connector is turned into its own internal subgraph, and the federated query graph has a root type edge between every pair of subgraphs. Building on [PR #10358](https://github.com/apollographql/router/pull/10358), the non-trivial followup edges are now computed once per tail node and shared between all the edges that lead to the same list, instead of storing a separate copy for each edge. Building the root type edges also allocates less, and query graph edges are smaller.

With 1,600 connectors, composition drops from 2.1s and 1.4 GB to 1.3s and 770 MB; with 3,200 connectors, from 7.8s and 4.8 GB to 4.1s and 2.4 GB. The computed followup edges and their order are unchanged, so query plans are unaffected.

By [@dariuszkuc](https://github.com/dariuszkuc) in <https://github.com/apollographql/router/pull/10346>
