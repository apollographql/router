### `http.server.request.duration` now covers the full request lifecycle, plus a new opt-in `http.server.request.time_to_first_response` ([PR #9754](https://github.com/apollographql/router/pull/9754))

The standard `http.server.request.duration` histogram now measures the **entire** request lifecycle, from request start until the client-facing response stream closes, instead of stopping when the primary response is ready. For plain (non-streamed) responses this is unchanged. For `@defer` and subscription responses the recorded duration now includes the streamed tail that it previously missed.

The sample is recorded **exactly once** per request: when the response stream completes normally, or, if the client disconnects or the stream is cancelled mid-flight, at the moment the stream is dropped (using the elapsed time at that point). This guarantees a duration is always recorded, including for a client that opens a request and then hangs.

This is a behavior change to a widely-used metric. Dashboards and alerts built on `http.server.request.duration` for operations that use `@defer` or subscriptions will see larger values after upgrading, because the streamed tail is now included.

To preserve the previous measurement, a new standard instrument records the time to the first response:

- `http.server.request.time_to_first_response` records the time from request start until the router has the primary response ready to send (status, headers, and the first response chunk). This is exactly what `http.server.request.duration` measured before this change. It is measured inside the router, so it is not a client-observed time to first byte (it ends before response serialization and network transit). It is **opt-in** (disabled by default, even when `default_requirement_level` enables the other standard instruments), so upgrading does not add a new histogram unless you enable it.

By [@ebylund](https://github.com/ebylund) in https://github.com/apollographql/router/pull/9754
