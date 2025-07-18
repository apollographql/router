### fix: Remove use of APOLLO_TELEMETRY_DISABLED from the fleet detector plugin ([PR #7907](https://github.com/apollographql/router/pull/7907))


The `APOLLO_TELEMETRY_DISABLED` environment variable only disables anonymous telemetry, it was never meant for disabling identifiable telemetry. This includes metrics from the fleet detection plugin.

By [@DMallare](https://github.com/DMallare) in https://github.com/apollographql/router/pull/7907