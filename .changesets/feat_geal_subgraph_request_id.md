### Add a subgraph request id ([PR #5858](https://github.com/apollographql/router/pull/5858))

This is a unique string identifying a subgraph request and response, allowing plugins and coprocessors to keep some state per subgraph request by matching on this id. It is available in coprocessors as `subgraphRequestId` and rhai scripts as `request.subgraph.id` and `response.subgraph.id`.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5858