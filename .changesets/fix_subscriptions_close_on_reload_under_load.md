### Close subscriptions promptly on schema and configuration reloads under heavy event traffic ([PR #10857](https://github.com/apollographql/router/pull/10857))

Subscriptions now close with a `SUBSCRIPTION_SCHEMA_RELOAD` or `SUBSCRIPTION_CONFIG_RELOAD` error as soon as a reload completes, even while the subgraph sends events continuously. Previously such a subscription kept running on the previous schema, holding it in memory, until the events paused or the connection was force-closed after `connection_shutdown_timeout`. A configuration reload also used to send its error and then keep delivering events; it now ends the subscription.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10857
