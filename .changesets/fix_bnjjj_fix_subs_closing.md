### Fix deduplication and websocket stream termination ([PR #8104](https://github.com/apollographql/router/pull/8104))

Fixes an issue where WebSocket connections to subgraphs would remain open after all client subscriptions were closed. This could lead to unnecessary resource usage and connections not being properly cleaned up until a new event was received.

Previously, when clients disconnected from subscription streams, the router would correctly close client connections but would leave the underlying WebSocket connection to the subgraph open indefinitely in some cases.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8104
