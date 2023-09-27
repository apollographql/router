### Update to OpenTelemetry 0.20.0 ([PR #3649](https://github.com/apollographql/router/pull/3649))

The router now uses OpenTelemetry 0.20.0. This includes a number of fixes and improvements from upstream.

In particular metrics have some significant changes:
* Prometheus metrics are now aligned with the [OpenTelemetry spec](https://opentelemetry.io/docs/specs/otel/compatibility/prometheus_and_openmetrics/), and will not report `service_name` on each individual metric. Resource attributes are now moved to a single `target_info` metric.

  Users should check that their dashboards and alerts are properly configured when upgrading.

* The default service name for metrics is now `unknown_service` as per the [OpenTelemetry spec](https://opentelemetry.io/docs/concepts/sdk-configuration/general-sdk-configuration/#otel_service_name).

  Users should ensure to configure service name via router.yaml, or via the `OTEL_SERVICE_NAME` environment variable.

* The order of priority for setting service name has been brought into line with the rest of the router configuration. The order of priority is now:
  1. `OTEL_RESOURCE_ATTRIBUTES` environment variable
  2. `OTEL_SERVICE_NAME` environment variable
  3. `resource_attributes` in router.yaml
  4. `service_name` in router.yaml

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/3649
