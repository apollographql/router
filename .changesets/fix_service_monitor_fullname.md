### Align `ServiceMonitor` naming with other chart resources using the `router.fullname` helper ([Issue #TSH-22160](https://github.com/apollographql/router/issues/TSH-22160))

The `ServiceMonitor` Helm resource was using `.Release.Name` directly as its `metadata.name`, while all other chart resources (e.g. `Service`, `Deployment`) already used the `router.fullname` helper. This caused a naming inconsistency: for a release named `my-release`, the `Service` would be named `my-release-router` but the `ServiceMonitor` would be named `my-release`.

This change aligns the `ServiceMonitor` name with the rest of the chart by using `{{ include "router.fullname" . }}`, ensuring consistent naming and proper support for `nameOverride` and `fullnameOverride` values.

By [@mateusgoettems](https://github.com/mateusgoettems) in https://github.com/apollographql/router/pull/8929
