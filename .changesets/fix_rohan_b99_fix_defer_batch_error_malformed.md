### Ensure defer + batch query error is sent as single json response instead of malformed multipart body ([PR #9311](https://github.com/apollographql/router/pull/9311))

Batched queries that use defer are not supported by the router, we now return a single response with errors to indicate this.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9311
