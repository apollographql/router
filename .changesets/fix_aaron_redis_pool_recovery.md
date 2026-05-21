### Fix Redis cache stalls and harden the `required_to_start` startup contract

Resolves a class of failure where the router's Redis-backed caches (query planner, entity cache, APQ, response caching) would silently stall after a network event involving Redis replicas or the full cluster, accumulating queued commands, command timeouts, latency, and memory pressure until the router was redeployed.  Also hardens the `required_to_start: true` startup contract so it actually fails fast when Redis is unreachable, instead of hanging indefinitely or silently downgrading to best-effort.

**What was happening — Redis cache stalls**

The Redis client (`fred`) was configured with its defaults around reconnection: it ignored reconnection errors and retried indefinitely.  When an OS-level I/O error broke a primary→replica connection, `fred` could lose that connection permanently — falling back to the primary, clearing its routing table, and remaining blind to replicas that were still in the cluster topology.  In high-availability deployments where the broadcast topology contains nodes that aren't routeable from the router's network position (e.g. internal IPs reserved for replica promotion), this behavior caused unbounded reconnect attempts that blocked the tasks running the router's Redis client.

**What was happening — `required_to_start` could hang or silently succeed**

When `supergraph.query_planning.cache.redis.required_to_start` (or the corresponding APQ / entity cache / response cache flag) is set to `true` and Redis is unreachable, the router's startup path could either hang indefinitely or, under broadcast overflow, silently return success without verifying any successful connect.  Both modes weakened the documented fail-fast contract.  Two related bugs were involved:

- **Subscribe-after-publish race** — `RedisCacheStorage::create_client_pool` called `client_pool.connect_pool()` (which spawns fred's connection tasks immediately) and then `client_pool.wait_for_connect().await`.  `wait_for_connect` subscribes to fred's per-client `connect` broadcast channel at the moment it is `.await`ed — but the broadcast channel does not buffer events for late subscribers.  If fred's bounded reconnect policy exhausted its 15 attempts before the subscription was registered, the terminal `broadcast_connect(Err(..))` was fired into the void, fred aborted the pool, and the subscriber waited forever for an event that would never come.  This surfaced as a 120 s timeout on the `connection_failure_blocks_startup` integration test on contended CI runners.
- **`Lagged` arm silently dropped receivers** — the post-subscribe `recv()` loop's `RecvError::Lagged` arm used `continue` directly inside the `match`.  The innermost enclosing loop was the outer `for mut rx in connect_rxs`, so `continue` advanced to the *next* receiver and dropped the lagged one without ever observing its connect event.  Under extreme load (broadcast queue overflow between subscribe and recv) every rx could lag in turn, in which case the function returned `Ok(())` without observing any successful connect — silently downgrading `required_to_start: true` to best-effort.

**What changed**

The Redis integration now has four cooperating safety mechanisms for the steady-state recovery path, plus two fixes that make `required_to_start: true` honor its fail-fast contract:

1. **`RouteableReplicaFilter`** — a new replica filter that runs a 250ms TCP probe against each replica before `fred` opens a connection.  Unrouteable replicas are screened out of the routing table entirely.  Results are cached per replica for 5 minutes.  This prevents broadcast-but-unrouteable replicas from ever entering the routing table.
2. **Lazy replica connections** — `lazy_connections = true` is now explicit (matches `fred`'s default but is pinned).  With eager connections, a replica connection failure during topology sync can propagate pool-wide; lazy connections defer and isolate failures to per-command time.
3. **Bounded reconnect policy** — `fred`'s reconnection attempts are now bounded to 15 exponential-backoff attempts (1ms base, 2000ms max, factor 5) with `ignore_reconnection_errors = false`.  After 15 attempts, `fred` aborts and signals the router instead of retrying forever.
4. **Router-managed pool recreation** — when `fred` aborts, a watcher task clears the pool and the next request triggers a full pool rebuild.  Recreation is admission-controlled with `try_lock_owned()` so exactly one rebuild is in flight at a time; concurrent callers receive a fast-fail error rather than each spawning a redundant rebuild.  The owned guard is moved into the recreation task, ensuring the lock is held for the duration of the rebuild rather than just for the duration of `spawn()`.
5. **Subscribe-before-connect ordering** — `create_client_pool` now mirrors fred's own `Pool::init` shape: when `required_to_start` is set, the per-client connect-notification receivers are collected *before* `connect_pool()` is called, then awaited one at a time.  The broader connection lifecycle (separate per-client `ConnectHandle`s for the watcher task, reconnect policy, pool recreation) is unchanged.
6. **`Lagged` retries on the same receiver** — the `RecvError::Lagged` arm now wraps the `match` in an inner `loop { ... }` so `continue` re-awaits on the same receiver, matching the comment's original intent.

The router also now supports `required_to_start: false`, allowing the router to start successfully when Redis is unavailable at boot and to begin caching once Redis becomes reachable.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9023 and https://github.com/apollographql/router/pull/9418
