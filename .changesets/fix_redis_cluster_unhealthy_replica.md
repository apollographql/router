### One unhealthy replica no longer fails a Redis Cluster connection

In Redis Cluster mode the router connects to every replica up front and treats a failed replica connection as fatal. A replica that accepted connections but then closed them or never answered (for example a paused, overloaded or restarting replica) failed the whole connection. With `required_to_start` the router refused to start. Otherwise the router recreated its connection pool in a loop, logging `redis client aborted; marking for recreation`, and Redis caching stopped working even though every primary was healthy.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10705

By [@BrynCooke](https://github.com/BrynCooke)
