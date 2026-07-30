### Answer with a 503 when the router's request queue is full ([PR #9799](https://github.com/apollographql/router/pull/9799))

A saturated router left incoming requests waiting for capacity. It now responds `503 Service Unavailable` with the `REQUEST_CONCURRENCY_LIMITED` error code once its request queue is full, so clients get an answer instead of waiting.

Requests answered this way are recorded in the `http.server.request.duration` metric, alongside the requests that reached the router pipeline.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/9799
