### Propagate trace context on WebSocket upgrade requests ([PR #8739](https://github.com/apollographql/router/pull/8739))

The router now injects trace propagation headers into the initial HTTP upgrade request when it opens WebSocket connections to subgraphs. This preserves distributed trace continuity between the router and subgraph services.

Trace propagation happens during the HTTP handshake only. After the WebSocket connection is established, headers cannot be added to individual messages.

By [@theJC](https://github.com/theJC) in https://github.com/apollographql/router/pull/8739