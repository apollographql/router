### Add subscription and defer observability: end reason span attributes and termination metrics ([PR #8858](https://github.com/apollographql/router/pull/8858))

Adds new span attributes and metrics to improve observability of streaming responses.

**Span attributes:**

- **`apollo.subscription.end_reason`**: Records the reason a subscription was terminated. Possible values are `server_close`, `stream_end`, `heartbeat_delivery_failed`, `client_disconnect`, `schema_reload`, and `config_reload`.
- **`apollo.defer.end_reason`**: Records the reason a deferred query ended. Possible values are `completed` (all deferred chunks were delivered successfully) and `client_disconnect` (the client disconnected before all deferred data was delivered).

Both attributes are added dynamically to router spans only when relevant (i.e., only on requests that actually use subscriptions or `@defer`), rather than being present on every router span.

**Metrics:**

The following counters are emitted when a subscription terminates:

- **`apollo.router.operations.subscriptions.stream_end`** (attributes: `subgraph.service.name`): The subgraph gracefully closed the stream.
- **`apollo.router.operations.subscriptions.subgraph_error`** (attributes: `subgraph.service.name`): The subscription terminated unexpectedly due to a subgraph error (e.g. process killed, network drop).
- **`apollo.router.operations.subscriptions.client_disconnect`** (attributes: `apollo.client.name`): The client disconnected before the subscription ended.
- **`apollo.router.operations.subscriptions.heartbeat_delivery_failed`** (attributes: `apollo.client.name`): A heartbeat could not be delivered to the client.
- **`apollo.router.operations.subscriptions.schema_reload`**: The subscription was terminated because the router schema was updated.
- **`apollo.router.operations.subscriptions.config_reload`**: The subscription was terminated because the router configuration was updated.

The following counter is emitted when a subscription request is rejected:

- **`apollo.router.operations.subscriptions.rejected.limit`**: A new subscription request was rejected because the router has reached its `max_opened_subscriptions` limit.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8858