### Add subgraph request id ([PR #5858](https://github.com/apollographql/router/pull/5858))

The router now supports a subgraph request ID that is a unique string identifying a subgraph request and response. It allows plugins and coprocessors to keep some state per subgraph request by matching on this ID. It's available in coprocessors as `subgraphRequestId` and Rhai scripts as `request.subgraph.id` and `response.subgraph.id`.


By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5858