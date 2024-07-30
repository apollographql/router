### Use spawn_blocking for query parsing and validation ([PR #5235](https://github.com/apollographql/router/pull/5235))

To prevent its executor threads from blocking on large queries, the router now runs query parsing and validation in a Tokio blocking task.

By [@xuorig](https://github.com/xuorig) in https://github.com/apollographql/router/pull/5235