### Fix the query hashing algorithm ([PR #6205](https://github.com/apollographql/router/pull/6205))

> [!IMPORTANT]
> If you have enabled [Distributed query plan caching](https://www.apollographql.com/docs/router/configuration/distributed-caching/#distributed-query-plan-caching), updates to the query planner in this release will result in query plan caches being re-generated rather than re-used.  On account of this, you should anticipate additional cache regeneration cost when updating between these versions while the new query plans come into service.

The Router includes a schema-aware query hashing algorithm designed to return the same hash across schema updates if the query remains unaffected. This update enhances the algorithm by addressing various corner cases, improving its reliability and consistency.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/6205
