### Entity cache: Add spans around redis interaction ([PR #4667](https://github.com/apollographql/router/pull/4667))

This adds the `cache_lookup` and `cache_store` spans to show the entity cache's Redis calls in traces. This also changes the behavior slightly so that storing in Redis does not stop the execution of the rest of the query

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4667