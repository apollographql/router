### Batching metrics no longer count invalid batches ([PR #10852](https://github.com/apollographql/router/pull/10852))

When a batch request contains a JSON structure that cannot be parsed as a GraphQL request, the Router rejects the whole batch with an `INVALID_GRAPHQL_REQUEST` error. Previously, such rejected requests were still counted in batching metrics, though they were never executed. The following metrics no longer include these requests:

- `apollo.router.operations.batching`
- `apollo.router.operations.batching.size`

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/10852
