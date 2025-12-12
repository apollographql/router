### Fix: Propagate trace context on websocket upgrade request ([PR #8739](https://github.com/apollographql/router/pull/8739))

GraphQL subscriptions in Apollo Router quite often leverage WebSocket connections to subgraphs. Currently, distributed trace context is not propagated when establishing these WebSocket connections, breaking trace continuity between the router and subgraph services. This results in disconnected traces in observability platforms like Datadog.

The Solution: Inject trace propagation headers into the initial HTTP upgrade request that establishes the WebSocket connection.

WebSocket trace propagation happens exclusively during the HTTP handshake. Once the WebSocket connection is established, headers cannot be added to individual messages—the WebSocket protocol does not support per-message headers.


By [@theJC](https://github.com/theJC) in https://github.com/apollographql/router/pull/8739