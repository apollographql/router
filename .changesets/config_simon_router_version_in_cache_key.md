### Add version number to distributed query plan cache keys ([PR #6406](https://github.com/apollographql/router/pull/6406))

The router now includes its version number in the cache keys of distributed cache entries. Given that a new router release may change how query plans are generated or represented, including the router version in a cache key enables the router to use separate cache entries for different versions.

If you have enabled [distributed query plan caching](https://www.apollographql.com/docs/router/configuration/distributed-caching/#distributed-query-plan-caching), expect additional processing for your cache to update for this router release.


By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/6406
