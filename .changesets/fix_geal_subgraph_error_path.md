### Set subgraph error path if not present ([PR #5773](https://github.com/apollographql/router/pull/5773))

The router now sets the error path in all cases during subgraph response conversion. Previously the router's subgraph service didn't set the error path for some network-level errors.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5773