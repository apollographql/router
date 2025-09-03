### Make the `id` field optional for WebSocket subscription `connection_error` messages ([Issue #6138](https://github.com/apollographql/router/issues/6138))

Fixed a Subscriptions over WebSocket issue where `connection_error` messages from subgraphs would be swallowed by the router because they incorrectly required an `id` field. According to the `graphql-transport-ws` specification (one of two transport specifications we provide support for), `connection_error` messages only require a `payload` field, **not** an `id` field. The `id` field in is now optional which will allow the underlying error message to propagate to clients when underlying connection failures occur.

By [@jeffutter](https://github.com/jeffutter) in https://github.com/apollographql/router/pull/8189