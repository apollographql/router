### Sending query planner metrics to Apollo ([PR #5267](https://github.com/apollographql/router/pull/5267))

To allow us to measure how much of an improvement the new query planner implementation makes, we are now transmitting metrics that start with:

`apollo.router.query_planning.*` to Apollo.

These metrics do not leak any sensitive information, but will greatly help us to improve query planning in the Router.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5267
