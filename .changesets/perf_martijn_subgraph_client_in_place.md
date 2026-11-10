### Call the subgraph HTTP client without copying its stack per request

The subgraph service copied the whole HTTP client stack beneath it for every subgraph request in order to call it from its response future. It now calls the client stack it has readied directly, inside the same `subgraph_request` span, so a subgraph request makes no copy of it. Requests, responses, spans and the `apollo.router.overhead` measurements are unchanged.

By [@martijnwalraven](https://github.com/martijnwalraven)
