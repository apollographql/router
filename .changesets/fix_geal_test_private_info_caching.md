### Entity cache fix: update the cache key with private info on the first call ([PR #5599](https://github.com/apollographql/router/pull/5599))

This adds a test for private information caching and fixes an issue where private data was stored at the wrong key, so it did not appear to be cached

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5599