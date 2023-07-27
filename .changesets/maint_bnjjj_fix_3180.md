### Don't reload the router if the schema/license hasn't changed ([Issue #3180](https://github.com/apollographql/router/issues/3180))

The router is performing frequent schema reloads due to notifications from uplink. In the majority of cases a schema reload is not required, because the schema hasn't actually changed.

We won't reload the router if the schema/license hasn't changed.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3478
