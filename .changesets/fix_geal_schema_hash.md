### do not hash the entire schema on every query plan cache lookup ([PR #5374](https://github.com/apollographql/router/pull/5374))

This fixes performance issues when looking up query plans for large schemas.

⚠️ Because this feature changes the query plan cache key, distributed caches will need to be repopulated.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5374