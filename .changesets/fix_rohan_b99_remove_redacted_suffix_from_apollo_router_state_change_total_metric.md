### Remove `_redacted` suffix from some event attributes for `apollo.router.state.change.total` metric ([Issue #8464](https://github.com/apollographql/router/pull/8464))

The previous implementation used the `Debug` trait implementation on `router::Event` to provide the event name for `apollo.router.state.change.total` metrics, which could look like `UpdateConfiguration(<redacted>)` and would show on APM platforms like `updateconfiguration_redacted`. This PR switches to a `Display` trait implementation which instead looks like `UpdateConfiguration` and `updateconfiguration` respectively. The custom behavior for `UpdateLicense` is retained, so that the license state name is still appended.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8464