### Report how often subgraph query deduplication shares a response ([PR #10391](https://github.com/apollographql/router/pull/10391))

Subgraphs with `traffic_shaping` `deduplicate_query` enabled now report `apollo.router.subgraph_deduplication.requests`. This counter has one entry for each subgraph request that reaches query deduplication. Its `apollo.router.subgraph_deduplication.outcome` attribute says what happened to the request:

- `leader`: the request was sent on.
- `follower`: the request reused the response of an identical in-flight request.
- `bypassed_batch`: the request is a query that is part of a batch, so it wasn't eligible.
- `bypassed_operation`: the request is a mutation or subscription, so it wasn't eligible, whether or not it is batched.

Each entry also carries `subgraph.name`.

Use the counter to see how much of your subgraph traffic deduplication actually saves. The metric is reported to your configured metrics exporters and to GraphOS. Subgraphs without query deduplication don't report it.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/10391
