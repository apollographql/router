### Reduce log level for interrupted WebSocket streams ([PR #8344](https://github.com/apollographql/router/pull/8344))

The router now logs interrupted WebSocket streams at `trace` level instead of `error` level.

Previously, WebSocket stream interruptions logged at `error` level, creating excessive noise in logs when clients disconnected normally or networks experienced transient issues. Client disconnections and network interruptions are expected operational events that don't require immediate attention.

Your logs will now be cleaner and more actionable, making genuine errors easier to spot. You can enable `trace` level logging when debugging WebSocket connection issues.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8344