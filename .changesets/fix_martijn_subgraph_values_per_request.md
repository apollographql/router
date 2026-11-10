### Each subgraph request reads its own response size limit and body sizes

The router kept three values of each subgraph request in the client request's shared context: the subgraph's `limits.subgraph` `http_max_response_size`, which the limits plugin wrote on every subgraph request, and the request and response body sizes, which the `subgraph_request_body_size` and `subgraph_response_body_size` telemetry selectors and the `http.client.request.body.size` and `http.client.response.body.size` instruments read. When a client request had two subgraph requests in flight at once, one could read the other's values: a subgraph's response was cut at another subgraph's limit, or not cut at its own, and a selector reported another request's size.

Each subgraph's limit is now read from the configuration when the router builds the subgraph's HTTP client, and the body sizes travel on each subgraph request's own response. A subgraph request no longer takes a lock on the shared context for them.

A connector's response is no longer cut by a subgraph's `http_max_response_size`, which a subgraph request of the same client request could leave in the shared context; a connector source's own `limits.connector` limit applies as before.

By [@martijnwalraven](https://github.com/martijnwalraven)
