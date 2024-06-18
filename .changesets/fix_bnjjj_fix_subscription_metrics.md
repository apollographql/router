### Inaccurate `apollo_router_opened_subscriptions` counter ([PR #5363](https://github.com/apollographql/router/pull/5363))

Fixes the `apollo_router_opened_subscriptions` counter which previously only incremented. The counter now also decrements.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5363