### Remove TPS rate limiting from license enforcement

The `license_enforcement` plugin no longer rate-limits traffic based on the transactions-per-second (TPS) claim in a router's license. Previously, requests exceeding the Free plan's TPS threshold were rejected with a `503` and a `ROUTER_FREE_PLAN_RATE_LIMIT_REACHED` GraphQL error. This enforcement is removed outright, with no replacement rate-limiting mechanism in the plugin.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/####
