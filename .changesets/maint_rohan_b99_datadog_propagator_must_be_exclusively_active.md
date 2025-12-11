### Warn when Datadog propagator isn't exclusively active ([PR #8677](https://github.com/apollographql/router/pull/8677))

The router now validates propagator configuration and emits a warning log if:
- The Datadog propagator is enabled and any other propagators are enabled (except baggage)
- Datadog tracing is enabled and other propagators are enabled (except baggage)

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8677
