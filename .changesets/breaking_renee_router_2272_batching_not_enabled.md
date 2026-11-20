### Remove `BATCHING_NOT_ENABLED` error ([PR #10852](https://github.com/apollographql/router/pull/10852))

When batching is not enabled and a client sends a batch request, the Router previously rejected the request with a `BATCHING_NOT_ENABLED` error.

Now, such a request will be rejected with a general `INVALID_GRAPHQL_REQUEST` error.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/10852