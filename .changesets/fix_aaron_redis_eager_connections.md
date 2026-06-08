### Fix Redis replica routing failure caused by lazy connections with even replica counts ([Issue/PR #9589](https://github.com/apollographql/router/pull/9589))

When a Redis cluster had an even number of replicas, the router's use of `lazy_connections = true` could trigger a bug in fred's round-robin replica selection logic. Fred increments its round-robin counter when searching for a routable replica, and increments it again when it can't find one before requeuing the command. With an even replica count this causes fred to consistently target replicas that have no established connection, leading to GET failures falling through to backends and Redis CPU spikes.

Switched to `lazy_connections = false` (eager connections) so all replica connections are established upfront. The `RouteableReplicaFilter` that was the original motivation for lazy connections — preventing unroutable replicas from entering the routing table — continues to handle that responsibility, making the blast-radius isolation that lazy connections provided redundant.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9589
