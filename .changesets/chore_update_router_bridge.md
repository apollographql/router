> [!IMPORTANT]
> If you have enabled [Distributed query plan caching](https://www.apollographql.com/docs/router/configuration/distributed-caching/#distributed-query-plan-caching), this release changes the hashing algorithm used for the cache keys.  On account of this, you should anticipate additional cache regeneration cost when updating between these versions while the new hashing algorithm comes into service.

### Update to Federation v2.9.1 ([PR #6029](https://github.com/apollographql/router/pull/6029))

This release updates to Federation v2.9.1, which fixes edge cases in subgraph extraction logic when using spec renaming or spec URLs (e.g., `specs.apollo.dev`) that could impact the planner's ability to plan a query.

By [@lrlna](https://github.com/lrlna) in https://github.com/apollographql/router/pull/6027
