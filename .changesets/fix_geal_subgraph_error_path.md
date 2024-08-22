### set the subgraph error path if not present ([PR #5773](https://github.com/apollographql/router/pull/5773))

This fixes subgraph response conversion to set the error path in all cases. For some network level errors, the subgraph service was not setting the path

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5773