### Supergraph instruments with a `response_context` value no longer log "this is a bug and should not happen"

A custom counter or histogram under `telemetry.instrumentation.instruments.supergraph` whose `value` is `response_context` logged an error such as `this is a bug and should not happen, the increment should only be Custom or EventCustom` for every response, and once per chunk for deferred responses. The recorded metrics were correct. The error is no longer logged.

This also fixes a counter recording its value twice for one request. It happened when the context value was removed before the response finished streaming and the counter had no `exists` condition on that value.

By [@rohan-b99](https://github.com/rohan-b99)
