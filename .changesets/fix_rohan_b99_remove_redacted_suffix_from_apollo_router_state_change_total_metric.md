### Remove `_redacted` suffix from event attributes in `apollo.router.state.change.total` metric ([Issue #8464](https://github.com/apollographql/router/issues/8464))

Event names in the `apollo.router.state.change.total` metric no longer include the `_redacted` suffix. The metric now uses the `Display` trait instead of `Debug` for event names, changing values like `updateconfiguration_redacted` to `updateconfiguration` in APM platforms.

The custom behavior for `UpdateLicense` events is retained—the license state name is still appended.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8464