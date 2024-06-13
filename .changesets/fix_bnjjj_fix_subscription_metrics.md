### `apollo_router_opened_subscriptions` counter was inaccurate ([PR #5363](https://github.com/apollographql/router/pull/5363))

The counter `apollo_router_opened_subscriptions` was only incrementing and never decrementing. Now it's handled properly.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5363