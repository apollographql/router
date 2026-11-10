### Build telemetry instrument sets once instead of on every subgraph request

The router built its subgraph instruments on every subgraph request: the standard `http.client.*` histograms, the custom instruments configured under `telemetry.instrumentation.instruments.subgraph`, the `apollo.router.response.cache` counter and Apollo's `apollo.router.operations.fetch.duration` histogram, copying each instrument's selectors and building Apollo's selector map each time. It now builds them once, when it builds each subgraph's service stack, and a subgraph request only starts the measurements it records into. Instrument values and attributes are unchanged.

By [@martijnwalraven](https://github.com/martijnwalraven)
