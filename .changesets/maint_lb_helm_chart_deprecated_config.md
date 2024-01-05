### Fixed update telemetry config in Helm chart ([PR #4360](https://github.com/apollographql/router/pull/4360))

Previously, if using the new `telemetry` configuration (notably `telemetry.exporters`), the Helm chart would result in both old and new configuration at the same time. This was invalid and prevented the router from starting up. In this release, the Helm chart outputs the appropriate structure based on user-provided configuration.

By [@lennyburdette](https://github.com/lennyburdette) in https://github.com/apollographql/router/pull/4360