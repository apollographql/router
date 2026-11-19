### Close subscriptions promptly on schema and configuration reloads under heavy event traffic

A subscription whose subgraph sent events continuously did not notice a schema or configuration reload until the events paused or the client connection was force-closed after `connection_shutdown_timeout`. Until then it kept serving from the previous schema, and the router kept that schema's pipeline in memory (visible as a lingering `apollo.router.pipelines` entry for the old schema). The router now closes these subscriptions as soon as the reload completes, with a `SUBSCRIPTION_SCHEMA_RELOAD` or `SUBSCRIPTION_CONFIG_RELOAD` error, however busy the subscription is.

A configuration reload now also ends the subscription, as documented. Previously the client received the `SUBSCRIPTION_CONFIG_RELOAD` error but events kept arriving on the same subscription afterwards. Clients that already reconnect on this error need no change.

By [@BrynCooke](https://github.com/BrynCooke)
