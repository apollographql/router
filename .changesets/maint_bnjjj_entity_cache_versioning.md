### Add version in the entity cache hash ([PR #5701](https://github.com/apollographql/router/pull/5701))

The hashing algorithm of the router's entity cache has been updated to include the entity cache version.

[!IMPORTANT]
If you have previously enabled [entity caching](https://www.apollographql.com/docs/router/configuration/entity-caching), you should expect additional cache regeneration costs when updating to this version of the router while the new hashing algorithm comes into service.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5701