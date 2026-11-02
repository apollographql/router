### One unhealthy replica no longer fails a Redis Cluster connection

In Redis Cluster mode the router connects to every replica up front and treats a failed replica connection as fatal. A replica that accepted connections but then closed them or never answered (for example a paused, overloaded or restarting replica) failed the whole connection. With `required_to_start` the router refused to start. Otherwise the router recreated its connection pool in a loop, logging `redis client aborted; marking for recreation`, and Redis caching stopped working even though every primary was healthy.

The router now checks each replica by opening a real connection to it, with the same credentials and TLS settings and within the configured Redis `timeout`. A replica that fails the check is left out and checked again after 5 minutes, so a replica that recovers gets reads back within about 5 minutes. Reads go to the remaining replicas, or to the primary when a shard has none. A replica that stops answering after the router connected is left out within about 10 seconds; until then, reads routed to that replica time out. Each time a replica is left out, the router logs a warning: `redis replica <address> failed its probe and won't receive reads until it's probed again in 300s`.

By [@BrynCooke](https://github.com/BrynCooke)
