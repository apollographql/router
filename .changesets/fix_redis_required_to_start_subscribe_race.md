### Fix `required_to_start: true` startup hang on unreachable Redis

When `supergraph.query_planning.cache.redis.required_to_start` (or the corresponding APQ / entity cache / response cache flag) is set to `true` and Redis is unreachable, the router's startup path could hang indefinitely instead of failing fast.

**What was happening**

`RedisCacheStorage::create_client_pool` called `client_pool.connect_pool()` (which spawns fred's connection tasks immediately) and then `client_pool.wait_for_connect().await`. `wait_for_connect` subscribes to fred's per-client `connect` broadcast channel at the moment it is `.await`ed — but the broadcast channel does not buffer events for late subscribers. If fred's bounded reconnect policy exhausted its 15 attempts before the subscription was registered, the terminal `broadcast_connect(Err(..))` was fired into the void, fred aborted the pool, and the subscriber waited forever for an event that would never come.

This was a subscribe-after-publish race that surfaced as a 120 s timeout on the `connection_failure_blocks_startup` integration test on contended CI runners.

**What changed**

`create_client_pool` now mirrors fred's own `Pool::init` shape: when `required_to_start` is set, the per-client connect-notification receivers are collected *before* `connect_pool()` is called. We then await each receiver in turn. The broader connection lifecycle (separate per-client `ConnectHandle`s for the watcher task, reconnect policy, pool recreation) is unchanged.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9412
