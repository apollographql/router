### Supergraph `event_custom` instruments valued from `response_context` or `static` now record once per response chunk

A custom counter or histogram under `telemetry.instrumentation.instruments.supergraph` whose `value` is `event_custom` with `response_context` or `static` is documented to record for every supergraph response payload, including @defer parts and subscription events. It recorded only once per request, using the value present when the response started, and histograms and `static` values logged `this is a bug and should not happen` for every chunk.

These instruments now record one sample per response chunk, each with the value read when that chunk is sent, and no longer log the error. Plain responses record the same values as before. Deferred responses and subscriptions record more: a counter reading 74 from the context on a query with two deferred fragments now adds 222 instead of 74, and its histogram records 3 samples instead of 1. A subscription with four events adds 296.

To keep one sample per request, remove the `event_custom` wrapper: change `value: { event_custom: { response_context: <key> } }` to `value: { response_context: <key> }`, and `value: { event_custom: { static: <n> } }` to `value: { static: <n> }`.

By [@rohan-b99](https://github.com/rohan-b99)
