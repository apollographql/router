### Add version in the entity cache hash ([PR #5701](https://github.com/apollographql/router/pull/5701))

[!IMPORTANT]
If you have enabled [entity caching](https://www.apollographql.com/docs/router/configuration/entity-caching), this release changes the hashing algorithm used for the cache keys.  On account of this, you should anticipate additional cache regeneration cost when updating between these versions while the new hashing algorithm comes into service.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5701