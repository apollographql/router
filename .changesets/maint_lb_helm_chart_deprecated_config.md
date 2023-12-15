### fix: update telemetry config in helm chart ([PR #4360](https://github.com/apollographql/router/pull/4360))

Before this change, if using the new `telemetry` configuration (notably `telemetry.exporters`), the helm chart would result in both old and new configuration at the same time. This is invalid and would prevent the router from starting up. With this change, the helm chart will output the appropriate structure based on user-provided configuration.

<!-- start metadata -->
---

By [@lennyburdette](https://github.com/lennyburdette) in https://github.com/apollographql/router/pull/4360