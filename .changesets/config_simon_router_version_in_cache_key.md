### Distributed query plan cache keys include the Router version number ([PR #6406](https://github.com/apollographql/router/pull/6406))

More often than not, an Apollo Router release may contain changes that affect what query plans are generated or how they’re represented. To avoid using outdated entries from distributed cache, the cache key includes a counter that was manually incremented with relevant data structure or algorithm changes. Instead the cache key now includes the Router version number, so that different versions will always use separate cache entries.

If you have enabled [Distributed query plan caching](https://www.apollographql.com/docs/router/configuration/distributed-caching/#distributed-query-plan-caching), starting with this release and going forward you should anticipate additional cache regeneration cost when updating between any Router versions.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/6406
