### Propagate client name and version modifications through telemetry ([PR #7369](https://github.com/apollographql/router/pull/7369))

The router accepts modifications to the client name and version (`apollo::telemetry::client_name` and `apollo::telemetry::client_version`), but those modifications are not currently propagated through the telemetry layers to update spans and traces.

This PR moves where the client name and version are bound to the span, so that the modifications from plugins **on the `router` service** are propagated.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7369
