### Evaluate telemetry instruments and usage reporting only where something reads them

The router's telemetry layers did their work on every request whether or not anything read it. Each part now runs where its reader is configured, read when the router builds its pipeline:

- Instruments, the standard `http.*` instruments, the custom instruments, the GraphQL, cost and response cache instruments and Apollo's fetch duration, and the per-request memory tracking behind `apollo.router.request.memory`, run only where a meter provider has a reader: a Prometheus or OTLP metrics exporter, or Apollo's metrics with an Apollo key and graph reference.
- Error counting (`apollo.router.operations.error`, `apollo.router.graphql_error` and the error span events) runs where metrics are read or spans are built.
- The router overhead tracker runs where metrics are read, an event is configured or spans are built.
- An operation's extended references and the enum values in its responses are collected only where Apollo usage reporting is configured, with an Apollo key and graph reference, in the extended reference mode. A router with no Apollo key no longer collects them.
- Where nothing reads anything, no metrics reader, no span built, no configured event and no usage reporting, the subgraph, connector, execution and HTTP client telemetry layers are left out of the service stacks, and the router and supergraph layers keep only the client library header check, the client's entries in the request context and the field-level instrumentation draw.

Where the saving is nil: a router with an Apollo key and graph reference has Apollo's metrics as a reader, so it keeps evaluating every instrument, custom instruments Apollo does not read included, and it builds spans for Apollo's traces. A router at its default log lines builds spans, so its layers stay in place, but it no longer evaluates instruments, tracks memory per request or collects extended references when it has no metrics exporter and no Apollo key.

A pipeline keeps what it read for its life: after a reload, requests the previous pipeline serves while the new one warms up are instrumented as the previous configuration says. Where errors are not counted, the `apollo::telemetry::counted_errors` context entry is not written.

By [@martijnwalraven](https://github.com/martijnwalraven)
