### Fix Redis cache stalls when replicas are unreachable or the cluster is unavailable

Resolves a class of failure where the router's Redis-backed caches (query planner, entity cache, APQ, response caching) would silently stall after a network event involving Redis replicas or the full cluster, accumulating queued commands, command timeouts, latency, and memory pressure until the router was redeployed.

**What was happening**

The Redis client (`fred`) was configured with its defaults around reconnection: it ignored reconnection errors and retried indefinitely. When an OS-level I/O error broke a primary→replica connection, `fred` could lose that connection permanently — falling back to the primary, clearing its routing table, and remaining blind to replicas that were still in the cluster topology. In high-availability deployments where the broadcast topology contains nodes that aren't routeable from the router's network position (e.g. internal IPs reserved for replica promotion), this behavior caused unbounded reconnect attempts that blocked the tasks running the router's Redis client.

**What changed**

This change adds four cooperating safety mechanisms to the router's Redis integration:

1. **`RouteableReplicaFilter`** — a new replica filter that runs a 250ms TCP probe against each replica before `fred` opens a connection. Unrouteable replicas are screened out of the routing table entirely. Results are cached per replica for 5 minutes. This prevents broadcast-but-unrouteable replicas from ever entering the routing table.
2. **Lazy replica connections** — `lazy_connections = true` is now explicit (matches `fred`'s default but is pinned). With eager connections, a replica connection failure during topology sync can propagate pool-wide; lazy connections defer and isolate failures to per-command time.
3. **Bounded reconnect policy** — `fred`'s reconnection attempts are now bounded to 15 exponential-backoff attempts (1ms base, 2000ms max, factor 5) with `ignore_reconnection_errors = false`. After 15 attempts, `fred` aborts and signals the router instead of retrying forever.
4. **Router-managed pool recreation** — when `fred` aborts, a watcher task clears the pool and the next request triggers a full pool rebuild. Recreation is admission-controlled with `try_lock_owned()` so exactly one rebuild is in flight at a time; concurrent callers receive a fast-fail error rather than each spawning a redundant rebuild. The owned guard is moved into the recreation task, ensuring the lock is held for the duration of the rebuild rather than just for the duration of `spawn()`.

The router also now supports `required_to_start: false`, allowing the router to start successfully when Redis is unavailable at boot and to begin caching once Redis becomes reachable.


By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9023
