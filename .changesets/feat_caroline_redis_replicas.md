### Support Redis read replicas ([PR #8405](https://github.com/apollographql/router/pull/8405))

Read-only queries will now be sent to replica nodes when using clustered Redis. Previously, all commands would be sent
to the master nodes.

This change applies to all Redis caches, including the query plan cache and the response cache.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/8405
