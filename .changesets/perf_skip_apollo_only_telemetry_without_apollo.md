### Skip Apollo-only telemetry work when the router doesn't report to Apollo ([PR #PULL_NUMBER](https://github.com/apollographql/router/pull/PULL_NUMBER))

The telemetry plugin did some work on every request and every subgraph or connector fetch that only Apollo GraphOS reporting reads. Without an Apollo key and graph ref, that output was thrown away. The router now skips this work unless it reports to Apollo:

- **FTV1 requests to subgraphs.** By default, 1% of requests sent `apollo-federation-include-trace: ftv1` to every subgraph. Subgraphs then built and returned field-level traces that nothing read. The router no longer asks for them, whatever `telemetry.apollo.field_level_instrumentation_sampler` is set to.
- **`apollo.router.operations.fetch.duration`.** This histogram goes only to Apollo, and the router no longer records it. When Apollo reporting is on, the router now builds the histogram's selectors once per subgraph or connector service instead of on every fetch, and no longer clones the Apollo config on every fetch.
- **Extended references.** The router no longer computes the input-field and enum references that `telemetry.apollo.metrics_reference_mode: extended` adds to usage reports.
- **Supergraph span variables.** The router no longer serializes each operation's variables onto the supergraph span for Apollo traces.

This changes nothing for routers that report to Apollo. Other exporters never received any of this data.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/PULL_NUMBER
