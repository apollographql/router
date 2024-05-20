### Expose busy timer APIs ([PR #4989](https://github.com/apollographql/router/pull/4989))

The router supports public APIs that native plugins can use to control when the router's busy timer is run.

The router's busy timer measures the time spent working on a request outside of waiting for external calls, like coprocessors and subgraph calls. It includes the time spent waiting for other concurrent requests to be handled (the wait time in the executor) to show the actual router overhead when handling requests.

The public methods are `Context::enter_active_request` and `Context::busy_time`.  The result is reported in the `apollo_router_processing_time` metric

For details on using the APIs, see the documentation for [`enter_active_request`](https://www.apollographql.com/docs/router/customizations/native#enter_active_request).

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4989