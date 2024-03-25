### Entity cache: fix support for Redis cluster ([PR #4790](https://github.com/apollographql/router/pull/4790))

in a Redis cluster, entities can be stored in different nodes, and a query to one node should only refer to the keys it manages. This is challenging for the MGET operation which requests multiple entities in the same request from the same node. This splits the MGET query in multiple MGETs calls grouped by key hash, to make sure each one will get to the corresponding node, then merges responses in the correct order.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4790