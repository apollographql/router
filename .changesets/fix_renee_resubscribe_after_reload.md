### Fix memory spikes and ordering with subscriptions and hot reload ([PR #7746](https://github.com/apollographql/router/pull/7746))

When a hot reload is triggered by a configuration change, the router attempted to apply updated configuration to open subscriptions. But this could cause memory spikes and excessive logging.

When a hot reload is triggered by a schema change, the router closed subscriptions with a `SUBSCRIPTION_SCHEMA_RELOAD` error. But this happened *before* the new schema was fully active and warmed up, so clients could reconnect tothe _old_ schema.

To fix these issues, a configuration and a schema change now have the same behavior. The router waits for the new configuration and schema to be active, and then closes all subscriptions with a `SUBSCRIPTION_SCHEMA_RELOAD` error, so clients can reconnect.

By [@goto-bus-stop](https://github.com/goto-bus-stop) and [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/7746