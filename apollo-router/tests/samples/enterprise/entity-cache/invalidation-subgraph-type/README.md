# Entity cache invalidation

This tests subgraph response extension based cache invalidation. This is the expected process:
- a query is sent to the "accounts" subgraph and cached
- we reload the subgraph with a mock mutation where the response has an extension to invalidate all data from the "accounts" subgraph
- we do the same query, we should get the same result as the first time (getting data from the cache instead of the subgraph)
- we do the mutation
- we reload the subgraph with a mock of the same query as precedently, but returning a different result
- the query is sent again by the client, we should get the new result now