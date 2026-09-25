### Unresponsive replicas no longer break non-cluster Redis caching

Since router 2.15.1, a non-cluster Redis deployment with replicas (for example, a primary with read replicas, or a managed high-availability instance with a standby) could lose its Redis cache entirely while the primary was healthy. This happened when a replica accepted TCP connections but never answered, such as a paused replica or a standby that doesn't accept clients. The router only reads from replicas in cluster mode, but it still opened a connection to every replica the primary reported and treated a failed replica connection as fatal. Each connection attempt stalled on the replica until it timed out, then the router recreated the whole connection pool, in a loop. The logs repeated `redis client aborted; marking for recreation` and `client pool being recreated after connection interrupt`, and cache reads and writes to the primary failed.

The eager, strictly monitored replica connections that work around a replica round-robin bug now apply only to Redis Cluster. As before 2.15.1, non-cluster connections leave replica connections lazy, so the router never opens connections to replicas it doesn't use, and a replica outage no longer affects the primary.

By [@BrynCooke](https://github.com/BrynCooke)
