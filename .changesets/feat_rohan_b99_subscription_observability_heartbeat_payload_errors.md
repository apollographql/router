### Add subscription and defer observability: end reason span attributes and termination metrics ([PR #8858](https://github.com/apollographql/router/pull/8858))

Adds new span attributes and metrics to improve observability of streaming responses.

**Span attributes:**

- **`apollo.subscription.end_reason`**: Records the reason a subscription was terminated. Possible values are `server_close`, `subgraph_error`, `heartbeat_delivery_failed`, `client_disconnect`, `schema_reload`, and `config_reload`.
- **`apollo.defer.end_reason`**: Records the reason a deferred query ended. Possible values are `completed` (all deferred chunks were delivered successfully) and `client_disconnect` (the client disconnected before all deferred data was delivered).

Both attributes are added dynamically to router spans only when relevant (i.e., only on requests that actually use subscriptions or `@defer`), rather than being present on every router span.

**Metrics:**

A single counter is emitted when a subscription terminates:

- **`apollo.router.operations.subscriptions.terminated.client`** (default attributes: `reason`, `subgraph.name`): Incremented once per client connection when a subscription stream ends. The `reason` attribute indicates why (possible values: `server_close`, `subgraph_error`, `client_disconnect`, `heartbeat_delivery_failed`, `schema_reload`, `config_reload`). The `subgraph.name` attribute is populated if available. When deduplication is enabled, a single subgraph WebSocket closure produces one `terminated` event per deduplicated client sharing that connection (each with `reason=server_close`).

  Attributes for this metric are configurable. By default, `reason` and `subgraph.name` are enabled. You can also enable `client.name` via configuration:

  ```yaml
  telemetry:
    instrumentation:
      instruments:
        router:
          apollo.router.operations.subscriptions.terminated.client:
            attributes:
              reason: true
              subgraph.name: true
              client.name: true
  ```

The following counter is emitted when a subscription request is rejected:

- **`apollo.router.operations.subscriptions.rejected`** (attributes: `reason`, `subgraph.name`): A subscription request was rejected. The `reason` attribute indicates why: `max_opened_subscriptions_limit_reached` (the router has reached its `max_opened_subscriptions` limit) or `subgraph` (the subgraph WebSocket connection failed, e.g. connection refused, protocol error, or failed subscription handshake). The `subgraph.name` attribute is populated when available, and defaults to an empty string otherwise.

The following counter is emitted when a subgraph ends a subscription:

- **`apollo.router.operations.subscriptions.terminated.subgraph`** (attributes: `subgraph.name`): Incremented once per subgraph WebSocket closure. Each deduplicated client sharing that connection will also emit a corresponding `apollo.router.operations.subscriptions.terminated.client` event with `reason=server_close`.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8858