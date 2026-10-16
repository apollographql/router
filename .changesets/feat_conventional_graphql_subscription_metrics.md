### Add opt-in `graphql.server.subscription.*` metrics from the draft OpenTelemetry GraphQL convention

Apollo Router can now emit two subscription metrics under OpenTelemetry's own [draft GraphQL semantic convention](https://github.com/open-telemetry/semantic-conventions/pull/3515): `graphql.server.subscription.event_count`, a counter of subscription events processed, and `graphql.server.subscription.event.duration`, a histogram of the time from receiving an event to writing its response.

The convention is still an open pull request at `development` stability, so these names may change before it's ratified. They stay off by default. Set `OTEL_SEMCONV_STABILITY_OPT_IN=graphql` or `OTEL_SEMCONV_STABILITY_OPT_IN=graphql/dup` to turn them on; the router treats both tokens the same way, since it has no existing metric to replace or duplicate against.

On `event_count`, the `error.type` attribute carries the event's first GraphQL error code. The router's own termination notices — schema reload, config reload, maximum lifetime, JWT expiry — are counted without it, because the router ended those subscriptions deliberately rather than failing to process an event. Use `apollo.router.operations.subscriptions.terminated.client` and its `reason` attribute to observe terminations.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/10223
