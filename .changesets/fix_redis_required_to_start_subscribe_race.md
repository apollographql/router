### Harden `required_to_start: true` fail-fast contract for Redis startup

When `supergraph.query_planning.cache.redis.required_to_start` (or the corresponding APQ / entity cache / response cache flag) is set to `true` and Redis is unreachable, the router's startup path could either hang indefinitely or, under broadcast overflow, silently return success without verifying any successful connect. Both modes weakened the documented fail-fast contract. Two related bugs were fixed.

**Bug 1 — subscribe-after-publish race on unreachable Redis**

`RedisCacheStorage::create_client_pool` called `client_pool.connect_pool()` (which spawns fred's connection tasks immediately) and then `client_pool.wait_for_connect().await`. `wait_for_connect` subscribes to fred's per-client `connect` broadcast channel at the moment it is `.await`ed — but the broadcast channel does not buffer events for late subscribers. If fred's bounded reconnect policy exhausted its 15 attempts before the subscription was registered, the terminal `broadcast_connect(Err(..))` was fired into the void, fred aborted the pool, and the subscriber waited forever for an event that would never come. This surfaced as a 120 s timeout on the `connection_failure_blocks_startup` integration test on contended CI runners.

Fix: `create_client_pool` now mirrors fred's own `Pool::init` shape. When `required_to_start` is set, the per-client connect-notification receivers are collected *before* `connect_pool()` is called, then awaited one at a time. The broader connection lifecycle (separate per-client `ConnectHandle`s for the watcher task, reconnect policy, pool recreation) is unchanged.

**Bug 2 — `Lagged` arm silently dropped receivers without verifying connect**

The post-subscribe `recv()` loop's `RecvError::Lagged` arm used `continue` directly inside the `match`. The innermost enclosing loop was the outer `for mut rx in connect_rxs`, so `continue` advanced to the *next* receiver and dropped the lagged one without ever observing its connect event. Under extreme load (broadcast queue overflow between subscribe and recv) every rx could lag in turn, in which case the function returned `Ok(())` without observing any successful connect — silently downgrading `required_to_start: true` to best-effort.

Fix: wrap the match in an inner `loop { ... }` so `Lagged`'s `continue` re-awaits on the same receiver, matching the comment's original intent.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9418
