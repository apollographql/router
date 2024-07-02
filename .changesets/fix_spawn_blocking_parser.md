### use spawn_blocking for query parsing & validation ([PR #5235](https://github.com/apollographql/router/pull/5235))

Moves query parsing and validation in a tokio blocking task to prevent all executor threads from blocking on large queries.

By [@xuorig](https://github.com/xuorig) in https://github.com/apollographql/router/pull/5235