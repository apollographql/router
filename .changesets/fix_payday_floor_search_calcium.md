### Rate limit errors emitted from open telemetry ([Issue #2953](https://github.com/apollographql/router/issues/2953))

When a batch span exporter is unable to send accept a span because the buffer is full it will emit an error.
These errors can be very frequent and could potentially impact performance.

Otel errors are now rate limited to one every ten seconds per error type.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2954
