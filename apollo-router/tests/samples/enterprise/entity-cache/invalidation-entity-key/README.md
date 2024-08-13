# Entity cache invalidation

This tests entity cache invalidation based on entity keys. This is the expected process:
- a query is sent to the router, for which multiple entities will be requested
- we reload the subgraph with a mock mutation where the response has an extension to invalidate one of the entities
- we do the same query, we should see an `_entities` query that only requests that specific entity