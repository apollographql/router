### Each subgraph and connector source is held only to its own response size limit ([PR #10917](https://github.com/apollographql/router/pull/10917))

An `http_max_response_size` limit set under `limits.subgraph` or `limits.connector` could be applied to subgraphs and connector sources it wasn't configured for. After a request fetched from a subgraph with a limit, its later or parallel fetches from subgraphs without one were cut off at that limit, and so were calls to connector sources. Connector sources had the same problem with `limits.connector`: one source's limit applied to other sources in the same request. The cut-off responses failed with "exceeded limit of N bytes" errors and counted towards `apollo.router.limits.subgraph_response_size.exceeded` or `apollo.router.limits.connector_response_size.exceeded`.

Each subgraph is now checked only against its own limit, or the `all` limit when it has none, and each connector source only against its own `limits.connector` limit or the connector `all` limit. Connector calls are never checked against a subgraph limit.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10917
