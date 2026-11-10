### Build telemetry instrument sets once instead of on every request

The router built its instruments on every request and every subgraph and connector request: the standard `http.*` instruments, the custom instruments configured under `telemetry.instrumentation.instruments`, the GraphQL, cost and response cache instruments and Apollo's `apollo.router.operations.fetch.duration` histogram, copying each instrument's selectors and conditions each time. It now builds them once, when it builds the pipeline's service stacks, and a request only starts the measurements it records into. A custom attribute's or event's name is also kept as a shared key, so recording an attribute no longer copies its name. Instrument values and attributes are unchanged.

By [@martijnwalraven](https://github.com/martijnwalraven)
