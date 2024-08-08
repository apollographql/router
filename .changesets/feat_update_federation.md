### Update federation to 2.8.3 ([PR #5781](https://github.com/apollographql/router/pull/5781))

> [!IMPORTANT]
> If you have enabled [Distributed query plan caching](https://www.apollographql.com/docs/router/configuration/distributed-caching/#distributed-query-plan-caching), this release changes the hashing algorithm used for the cache keys.  On account of this, you should anticipate additional cache regeneration cost when updating between these versions while the new hashing algorithm comes into service.

This updates the router from federation version 2.8.1 to 2.8.3. This updates addresses the following points:
- 

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5781