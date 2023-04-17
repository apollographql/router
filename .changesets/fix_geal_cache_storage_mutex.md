### use a parking-lot mutex in cache storage ([Issue #2751](https://github.com/apollographql/router/issues/2751))

The in memory cache requires synchronization and currently we use a futures aware mutex for that; but they are susceptible to contention. This replaces that mutex with a parking-lot synchronous mutex that is much faster.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2887