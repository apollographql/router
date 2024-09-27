> [!IMPORTANT]
> If you have enabled [Distributed query plan caching](https://www.apollographql.com/docs/router/configuration/distributed-caching/#distributed-query-plan-caching), this release changes the hashing algorithm used for the cache keys.  On account of this, you should anticipate additional cache regeneration cost when updating between these versions while the new hashing algorithm comes into service.

### Update to Federation v2.9.2 ([PR #6069](https://github.com/apollographql/router/pull/6069))

This release updates to Federation v2.9.2, with a small fix to the internal `__typename` optimization and a fix to prevent argument name collisions in the `@context`/`@fromContext` directives.

By [@dariuszkuc](https://github.com/dariuszkuc) in https://github.com/apollographql/router/pull/6069
