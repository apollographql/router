### Prevent sending internal `apollo_private.*` attributes to Jaeger collector ([PR #6033](https://github.com/apollographql/router/pull/6033))

When using the router's Jaeger collector to send traces, you will no longer receive span attributes with the `apollo_private.` prefix. Those attributes were incorrectly sent, as that prefix is reserved for internal attributes.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/6033