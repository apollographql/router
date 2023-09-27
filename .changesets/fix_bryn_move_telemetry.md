### Ensure that telemetry happens first ([Issue #3915](https://github.com/apollographql/router/issues/3915))

Telemetry related logic is now moved to the first thing in the router pipeline.

Previously the metric `apollo.router.operations` may have missed some requests if they were failed at the router stage.
In addition, some logic happened before root spans were created, which would have caused missing traces.

`apollo.router.operations` and root spans are now the first thing that happens in the router pipeline for graphql requests.



By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/3919
