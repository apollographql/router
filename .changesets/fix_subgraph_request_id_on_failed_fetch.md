### Keep the subgraph request ID on responses to failed subgraph fetches ([PR #10503](https://github.com/apollographql/router/pull/10503))

When the router couldn't reach a subgraph at all (for example, the connection was refused), the error response it produced had a newly generated subgraph request ID instead of the ID of the request. Rhai scripts and coprocessors that pair a `SubgraphRequest` with its `SubgraphResponse` by `subgraph_request_id` couldn't match that response to its request. The response now carries the request's ID, as other subgraph responses do.

By [@glasser](https://github.com/glasser) in https://github.com/apollographql/router/pull/10503
