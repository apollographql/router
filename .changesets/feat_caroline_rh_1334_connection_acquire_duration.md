### Add `apollo.router.connection.acquire.duration` metric for TCP/TLS connection timing ([PR #9309](https://github.com/apollographql/router/pull/9309))

Adds a new histogram metric, `apollo.router.connection.acquire.duration`, that records how long it takes to establish a new TCP or Unix socket connection to a downstream service (subgraph, connector, or coprocessor). The metric fires only when the connection pool opens a new connection — pool hits are not recorded.

This metric is useful for diagnosing connection establishment latency. For example, if a subgraph shows elevated overall response latency, a high `connection.acquire.duration` indicates the delay is in TCP/TLS setup; a near-zero value (or no data) points to post-connection causes like slow server responses.

Attributes:

- `network.transport`: `tcp` for HTTP connections, `unix` for Unix socket connections
- `subgraph.name`: name of the subgraph (for subgraph connections)
- `connector.source.name`: name of the connector source (for connector connections)
- `coprocessor`: `true` (for coprocessor connections)

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9309
