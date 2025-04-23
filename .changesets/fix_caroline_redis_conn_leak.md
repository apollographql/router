### Fix Redis connection leak ([PR #7319](https://github.com/apollographql/router/pull/7319))

The router performs a 'hot reload' whenever it detects a schema update. During this reload, it effectively instantiates a new internal router, warms it up (optional), redirects all traffic to this new router, and drops the old internal router.

This change fixes a bug in that drop process where the Redis connections are never told to terminate, even though the Redis client pool is dropped. This leads to an ever-increasing number of inactive Redis connections, which eats up memory.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7319
