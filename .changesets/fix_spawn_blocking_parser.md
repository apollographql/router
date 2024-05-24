### use spawn_blocking for query parsing & validation ([PR #5235](https://github.com/apollographql/router/pull/5235))

This PR runs query parsing and validation in a tokio blocking task to avoid blocking workers during longer
parse and validation times.

By [@xuorig](https://github.com/xuorig) in https://github.com/apollographql/router/pull/5235