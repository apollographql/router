### WebSocket connection cleanup for subscriptions ([PR #8104](https://github.com/apollographql/router/pull/8104))

WebSocket connections to subgraphs now close properly when all client subscriptions end, preventing unnecessary resource usage.

Previously, connections could remain open after clients disconnected, not being cleaned up until a new event was received. The router now tracks active subscriptions and closes the subgraph connection when the last client disconnects, ensuring efficient resource management.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8104
