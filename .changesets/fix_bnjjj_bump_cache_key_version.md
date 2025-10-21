### Update cache key version for entity caching ([PR #8458](https://github.com/apollographql/router/pull/8458))

> [!IMPORTANT]
>
> If you have enabled Entity caching, this release contains changes which necessarily alter the hashing algorithm used for the cache keys.  On account of this, you should anticipate additional cache regeneration cost when updating between these versions while the new hashing algorithm comes into service.

Bump entity cache key version to avoid keeping invalid cached data for too long fixed in #8456.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8458