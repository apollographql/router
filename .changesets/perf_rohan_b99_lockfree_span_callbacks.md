### Lower the CPU cost of the router's tracing spans ([PR #10644](https://github.com/apollographql/router/pull/10644))

The router's worker threads no longer wait on each other in its logging setup each time a request moves between internal tracing spans, which lowers its CPU time per request and raises throughput. Traces still contain every span.

You don't need to change any configuration. If your log filter (set with `--log`, `APOLLO_ROUTER_LOG` or `RUST_LOG`) uses a span-scoped directive such as `apollo_router[subgraph]=debug`, the router still does the extra work that directive needs, so the gain is smaller while it is set.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/10644
