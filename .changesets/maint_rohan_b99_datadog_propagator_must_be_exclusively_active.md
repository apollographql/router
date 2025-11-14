### Ensure datadog propagator is exclusively active ([PR #8514](https://github.com/apollographql/router/pull/8514))

Adds validation for propagator configuration, ensuring that:
- If the datadog propagator is enabled, no other propagators can be enabled (except baggage)
- If datadog tracing is enabled and other propagators are enabled, the datadog one must be disabled

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8514
