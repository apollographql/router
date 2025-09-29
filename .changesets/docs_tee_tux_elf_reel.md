### Clarify timeout hierarchy for traffic shaping ([PR #8203](https://github.com/apollographql/router/pull/8203))

The documentation reflects more clearly that subgraph timeouts should not be higher than the router timeout or the router timeout will initiate prior to the subgraph.

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/8203