### Do not include other fields from representation variable than the entity keys for entity cache([Issue #6673](https://github.com/apollographql/router/issues/6673))

Separate entity keys and representation variable value in the cache key to avoid issues with `@requires` for example.

close #6673

> [!IMPORTANT]
>
> If you have enabled [Distributed query plan caching](https://www.apollographql.com/docs/router/configuration/distributed-caching/#distributed-query-plan-caching), this release contains changes which necessarily alter the hashing algorithm used for the cache keys.  On account of this, you should anticipate additional cache regeneration cost when updating between these versions while the new hashing algorithm comes into service.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/6888