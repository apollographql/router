### Allow plugin test harness to use with_metrics for router_service ([PR #6655](https://github.com/apollographql/router/pull/6655))

This PR implements a custom future that allows the plugin test harness to work at the router_service using the
`with_metrics()` wrapper.
Without this new future you get a cryptic error that body is not Sync.
The custom future is only used for tests.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/6655
