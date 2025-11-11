### Support arrays in complex `@key` fields for entity caching ([PR #8367](https://github.com/apollographql/router/pull/8367))

Entity caching now supports arrays (including arrays of objects and scalars) in complex `@key` fields when resolving entities by key. This improves entity matching when using complex `@key` fields as primary cache keys.

By [@aaronArinder](https://github.com/aaronArinder), [@bnjjj](https://github.com/bnjjj), and [@duckki](https://github.com/duckki) in https://github.com/apollographql/router/pull/8367