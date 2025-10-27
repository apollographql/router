### Fix entity matching for complex `@key` fields when used with entity caching ([PR #8367](https://github.com/apollographql/router/pull/8367))

Improved entity matching for complex `@key` fields when used for entity caching primary cache key, adding support for arrays (including arrays of objects and scalars) when resolving entities by key.

By [@aaronArinder](https://github.com/aaronArinder) and [@bnjjj](https://github.com/bnjjj) and [@duckki](https://github.com/duckki) in https://github.com/apollographql/router/pull/8367