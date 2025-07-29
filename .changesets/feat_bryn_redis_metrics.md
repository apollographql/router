### Redis cache metrics ([PR #7920](https://github.com/apollographql/router/pull/7920))

The router now provides Redis cache monitoring with new metrics that help track performance, errors, and resource usage.

Connection and performance metrics:
  - `apollo.router.cache.redis.connections`: Number of active Redis connections
  - `apollo.router.cache.redis.command_queue_length`: Commands waiting to be sent to Redis, indicates if Redis is keeping up with demand
  - `apollo.router.cache.redis.commands_executed`: Total number of Redis commands executed
  - `apollo.router.cache.redis.redelivery_count`: Commands retried due to connection issues
  - `apollo.router.cache.redis.errors`: Redis errors by type, to help diagnose authentication, network, and configuration problems

**Experimental** performance metrics:
  - `experimental.apollo.router.cache.redis.network_latency_avg`: Average network latency to Redis
  - `experimental.apollo.router.cache.redis.latency_avg`: Average Redis command execution time  
  - `experimental.apollo.router.cache.redis.request_size_avg`: Average request payload size
  - `experimental.apollo.router.cache.redis.response_size_avg`: Average response payload size

> [!NOTE]
> The experimental metrics may change in future versions as we improve the underlying Redis client integration.

You can configure how often metrics are collected using the `metrics_interval` setting:

```yaml
supergraph:
  query_planning:
    cache:
      redis:
        urls: ["redis://localhost:6379"]
        ttl: "60s"
        metrics_interval: "1s"  # Collect metrics every second (default: 1s)
```

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/7920
