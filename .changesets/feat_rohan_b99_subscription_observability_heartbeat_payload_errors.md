### Add `apollo.subscription.end_reason` and `apollo.defer.end_reason` attributes to router spans ([PR #8858](https://github.com/apollographql/router/pull/8858))

Adds two new span attributes that indicate why a streaming response (subscription or defer) ended:

- **`apollo.subscription.end_reason`**: Records the reason a subscription was terminated. Possible values are `server_close`, `stream_end`, `heartbeat_delivery_failed`, `client_disconnect`, `schema_reload`, and `config_reload`.
- **`apollo.defer.end_reason`**: Records the reason a deferred query ended. Possible values are `completed` (all deferred chunks were delivered successfully) and `client_disconnect` (the client disconnected before all deferred data was delivered).

Both attributes are added dynamically to router spans only when relevant (i.e., only on requests that actually use subscriptions or `@defer`), rather than being present on every router span.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8858