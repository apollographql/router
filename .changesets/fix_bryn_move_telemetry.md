### Fix telemetry at the start of the router pipeline ([Issue #3915](https://github.com/apollographql/router/issues/3915))

Previously, the metric `apollo.router.operations` may have missed some requests if they were failed at the router stage. In addition, some logic happened before root spans were created, which would have caused missing traces.

Telemetry related logic is now moved to the first thing in the router pipeline, and `apollo.router.operations` and root spans are the first things that happen in the router pipeline for GraphQL requests.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/3919
