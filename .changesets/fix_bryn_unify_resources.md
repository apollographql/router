### Bring Otel `service.name` into line with the Otel spec ([PR #4034](https://github.com/apollographql/router/pull/4034))

Handling of Otel `service.name` has been brought into line with the [Otel spec](https://opentelemetry.io/docs/concepts/sdk-configuration/general-sdk-configuration/#otel_service_name) across traces and metrics.

Service name discovery is handled in the following order:
1. `OTEL_SERVICE_NAME` env
2. `OTEL_RESOURCE_ATTRIBUTES` env
3. `router.yaml` `service_name`
4. `router.yaml` `resources` (attributes)

If none of the above are found then the service name will be set to `unknown_service:apollo_router` or `unknown_service` if the executable name cannot be determined.

Users who have not explicitly configured their service name should do so either via the yaml config file or via the `OTEL_SERVICE_NAME` environment variable.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/4034
