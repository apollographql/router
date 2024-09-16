### Support Redis connection pooling ([PR #5942](https://github.com/apollographql/router/pull/5942))

This implements Redis connection pooling, for APQ, query planner and entity cache Redis usage. This can improve performance when there is some contention on the Redis connection, or some latency in Redis calls.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5942