### Remove TPS rate limiting and the `license_enforcement` config option

The router no longer rate-limits traffic based on the transactions-per-second (TPS) claim in a router's license. Previously, requests exceeding the Free plan's TPS threshold were rejected with a `503` and a `ROUTER_FREE_PLAN_RATE_LIMIT_REACHED` GraphQL error. This enforcement is removed outright, with no replacement rate-limiting mechanism.

The `license_enforcement` plugin had no other function, so it's been removed along with it. Configs containing `license_enforcement: {}` must remove that key, or the router will fail to start with a configuration error. Enforcement of an expired license (halting requests, and rate-limited logging of the expiry warning) is unaffected and still applies unconditionally.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/10123
