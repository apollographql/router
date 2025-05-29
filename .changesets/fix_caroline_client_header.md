### Propagate client name and version modifications through telemetry ([PR #7369](https://github.com/apollographql/router/pull/7369))

The Router accepts modifications to the client name and version (`apollo::telemetry::client_name` and `apollo::telemetry::client_version`), but those modifications were not propagated through the telemetry layers to update spans and traces.

After this change, the modifications from plugins **on the `router` service** are propagated through the telemetry layers.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7369
