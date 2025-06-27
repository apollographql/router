### Fix several hot reload issues with subscriptions ([PR #7746](https://github.com/apollographql/router/pull/7777))

When a hot reload is triggered by a configuration change, the router attempted to apply updated configuration to open subscriptions. This could cause excessive logging.

When a hot reload was triggered by a schema change, the router closed subscriptions with a `SUBSCRIPTION_SCHEMA_RELOAD` error.  This happened *before* the new schema was fully active and warmed up, so clients could reconnect to the _old_ schema, which should not happen.

To fix these issues, a configuration and a schema change now have the same behavior. The router waits for the new configuration and schema to be active, and then closes all subscriptions with a `SUBSCRIPTION_SCHEMA_RELOAD`/`SUBSCRIPTION_CONFIG_RELOAD` error, so clients can reconnect.

By [@goto-bus-stop](https://github.com/goto-bus-stop) and [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/7777
