### Run each subgraph request's service stack on the request's own task

Each subgraph's service stack sat behind a buffer whose single worker task readied and called the stack for every request to that subgraph, one after another, with a channel send, a oneshot and two task wake-ups per request. Each subgraph's stack is now shared as a pool of ready copies: a subgraph request takes a copy, readies and calls it on its own task and puts it back, so no worker task serializes the requests to a subgraph, and no copy of the stack is made per request.

The bound on requests to a subgraph that wait for its stack to become ready stays at 50,000, and a request past it still waits rather than failing. A subgraph stack whose readiness check fails now fails only the request that met the failure: the buffer failed that request and every later request to the subgraph until the router reloaded.

By [@martijnwalraven](https://github.com/martijnwalraven)
