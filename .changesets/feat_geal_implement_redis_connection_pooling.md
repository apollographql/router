### Support Redis connection pooling ([PR #5942](https://github.com/apollographql/router/pull/5942))

The router now supports Redis connection pooling for APQs, query planners and entity caches. This can improve performance when there is contention on Redis connections or latency in Redis calls.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5942