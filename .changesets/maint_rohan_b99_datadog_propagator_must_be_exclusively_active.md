### Warn if datadog propagator is not exclusively active ([PR #8677](https://github.com/apollographql/router/pull/8677))

Adds validation for propagator configuration, ensuring that a warning log is emitted if:
- The datadog propagator is enabled, and any other propagators are enabled (except baggage)
- Datadog tracing is enabled and other propagators are enabled (except for baggage)

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8677
