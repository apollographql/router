### Fix incorrect reference to `apollo.router.schema.load.duration` metric in the docs([PR #7581](https://github.com/apollographql/router/pull/7581))

The [in-memory cache documentation](https://www.apollographql.com/docs/graphos/routing/performance/caching/in-memory#cache-warm-up) was referencing an incorrect metric to track schema load times. Previously it was referred to as `apollo.router.schema.loading.time`, where the metric being emitted by the router since router@2.0.0 is `apollo.router.schema.load.duration`. This is now fixed in the docs.  

By [@lrlna](https://github.com/lrlna) in https://github.com/apollographql/router/pull/7581