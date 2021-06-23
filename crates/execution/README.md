# Execution
Defines GraphQLFetcher trait and implementations.
GraphQLFetcher takes a query and returns a stream of responses, one primary and any number of patches.

## Implementations
* Federated (IN PROGRESS) - Combines several subgraphs into a single federated response stream consisting of a primary 
  response an any number of patch responses.
* HttpSubgraphFetcher - Uses reqwest to fetch a stream of responses from a subgraph. For subgraphs that do not support 
  streaming a stream with a single primary response is returned.

The federated response stream is a series of flatmaps on subgraph queries. Streams from subgraphs are split into the 
primary response and a stream of patches.

To goal is to pass the caller a federated primary response, and a stream of federated patches.


