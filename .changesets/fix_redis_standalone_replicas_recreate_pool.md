### Unresponsive replicas no longer break non-cluster Redis caching

Since router 2.15.1, a non-cluster Redis deployment with replicas (for example, a primary with read replicas, or a managed high-availability instance with a standby) could lose its Redis cache entirely while the primary was healthy. This happened when a replica accepted connections but never answered, such as a paused replica or a standby that doesn't accept clients. The router repeatedly logged `redis client aborted; marking for recreation` and `client pool being recreated after connection interrupt`, and cache reads and writes to the primary failed.

Redis Cluster deployments are unchanged. Other Redis deployments no longer open connections to replicas, which the router doesn't read from outside cluster mode, so an unresponsive replica no longer affects caching on the primary. This restores the behavior from before 2.15.1.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/PULL_NUMBER
