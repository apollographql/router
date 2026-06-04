### `http.server.request.duration` now covers the full request lifecycle, plus new `http.server.request.time_to_first_response` ([PR #9269](https://github.com/apollographql/router/pull/9269))

The standard `http.server.request.duration` histogram now measures the **entire** request lifecycle — from request start until the client-facing response stream closes — instead of stopping when the response object is ready. This means the metric now includes the `@defer` / subscription tail that it previously missed.

To avoid losing the original time-to-first-byte signal, a new standard instrument records that measurement:

- `http.server.request.time_to_first_response` — time from request start to when the response is ready (the first byte). This is exactly what `http.server.request.duration` measured before this change.

  > Note: the final name of this metric is pending confirmation.

`http.server.request.duration` records its sample **exactly once**: when the response stream completes normally, or — if the client disconnects or the stream is cancelled mid-flight — at the moment the stream is dropped, using the elapsed time at that point. This guarantees a duration is always recorded, including for clients that open a request and then hang.

This change also removes the previously proposed `stream_duration` custom-instrument value: now that `http.server.request.duration` covers the full lifecycle, the separate opt-in instrument is redundant.

By [@ebylund](https://github.com/ebylund) in https://github.com/apollographql/router/pull/9269
