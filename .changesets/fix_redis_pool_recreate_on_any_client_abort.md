### Recreate the Redis client pool when any one client in it stops ([PR #10840](https://github.com/apollographql/router/pull/10840))

When a Redis client pool had more than one client (`pool_size` > 1), the router recreated the pool only after **every** client in it had stopped. If one client used up its reconnect attempts (for example, during a long Redis Sentinel failover) and the others reconnected, the pool kept sending that client its share of commands. Those commands timed out and, once the client's command buffer filled, failed with `Backpressure` errors ("Max in-flight commands reached.") until the router restarted.

The router now recreates the pool as soon as any one of its clients stops. Pools with `pool_size: 1` (the default) were not affected.

By [@Jeffery-Utter_pennent](https://github.com/Jeffery-Utter_pennent) in <https://github.com/apollographql/router/pull/10840>
