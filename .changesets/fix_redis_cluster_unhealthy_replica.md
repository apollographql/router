### One unhealthy replica no longer fails a Redis Cluster connection

In Redis Cluster mode the router connects to every replica up front and treats a failed replica connection as fatal. A replica that accepted connections but then closed them or never answered (for example a paused, overloaded or restarting replica) failed the whole connection. With `required_to_start` the router refused to start. Otherwise the router recreated its connection pool in a loop, logging `redis client aborted; marking for recreation`, and Redis caching stopped working even though every primary was healthy.

The router now checks each replica by opening a real connection to it, with the same credentials and TLS settings and within the configured Redis `timeout`. Replicas that fail the check are left out for at least 5 minutes, until the router next refreshes its list of replicas, which happens when a connection breaks or the cluster topology changes. Reads go to the remaining replicas, or to the primary when a shard has none. A replica that stops answering after the router connected is left out within about 10 seconds; until then, reads routed to that replica time out.

By [@BrynCooke](https://github.com/BrynCooke)
