### Instruments with a numeric `static` value no longer log "this is a bug and should not happen"

A custom counter or histogram under `telemetry.instrumentation.instruments.router`, `supergraph` or `subgraph` whose `value` is a numeric `static` logged an error such as `this is a bug and should not happen, the increment should only be Custom or EventCustom` once per request. The error is no longer logged.

Such an instrument with a `condition` that depends on the response, for example `eq: [200, response_status: code]`, previously recorded nothing. It now records its value once for each request that meets the condition, so you may see this metric for the first time after upgrading. Instruments with a `static` value and no response-dependent condition record the same values as before.

By [@rohan-b99](https://github.com/rohan-b99)
