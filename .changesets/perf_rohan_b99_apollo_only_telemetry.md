### Skip Apollo-only telemetry work when the router doesn't report to Apollo ([PR #PULL_NUMBER](https://github.com/apollographql/router/pull/PULL_NUMBER))

Without an Apollo key and graph ref, the router still did telemetry work on every request and fetch that only Apollo GraphOS reporting reads. It now skips that work, which lowers CPU and memory use per request:

- Requests no longer send the `apollo-federation-include-trace: ftv1` header to subgraphs, so subgraphs stop building field-level traces that nothing reads. By default 1% of requests sent it; this now holds whatever `telemetry.apollo.field_level_instrumentation_sampler` is set to.
- The router no longer records the `apollo.router.operations.fetch.duration` histogram, which only Apollo receives.
- The router no longer works out the extended references that `telemetry.apollo.metrics_reference_mode: extended` adds to usage reports, including the enum values in each response, or prepares operation variables for Apollo traces.

Telemetry from routers that report to Apollo is unchanged, and other exporters never received this data.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/PULL_NUMBER
