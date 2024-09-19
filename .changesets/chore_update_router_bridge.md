> [!IMPORTANT]
> If you have enabled [Distributed query plan caching](https://www.apollographql.com/docs/router/configuration/distributed-caching/#distributed-query-plan-caching), this release changes the hashing algorithm used for the cache keys.  On account of this, you should anticipate additional cache regeneration cost when updating between these versions while the new hashing algorithm comes into service.

### Update router-bridge@0.6.2+v2.9.1 ([PR #6027](https://github.com/apollographql/router/pull/6027))

Updates to latest router-bridge and federation version. This federation version fixes edge cases for subgraph extraction logic when using spec renaming or specs URLs that look similar to `specs.apollo.dev`

By [@lrlna](https://github.com/lrlna) in https://github.com/apollographql/router/pull/6027
