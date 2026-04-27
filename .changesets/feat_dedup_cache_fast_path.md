### Improve query plan cache throughput with an in-memory fast path ([PR #9273](https://github.com/apollographql/router/pull/9273))

Every query plan cache lookup — including cache hits — previously acquired the `wait_map` mutex before checking whether the value was in memory. On a warm cache this was pure overhead: the mutex was locked twice, a `broadcast::Sender` was allocated, and a cleanup task was spawned, all to be immediately discarded.

A fast path now checks the in-memory cache before acquiring the mutex. On a hit the value is returned immediately; the wait_map path is only entered on a miss, which is the only case where deduplication is needed.

By [@theJC](https://github.com/theJC) in https://github.com/apollographql/router/pull/9273
