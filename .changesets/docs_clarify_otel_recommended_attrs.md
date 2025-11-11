### Clarify guidance for OpenTelemetry "Recommended" attributes in telemetry documentation

The router telemetry documentation now clarifies that OpenTelemetry's "Recommended" attributes from their [development-status GraphQL semantic conventions](https://opentelemetry.io/docs/specs/semconv/graphql/graphql-spans/) are experimental and still evolving. Apollo recommends using `required` attributes instead of `recommended` attributes because of high cardinality, security, and performance risks with attributes like `graphql.document`.

Learn more in [Router Telemetry](https://www.apollographql.com/docs/graphos/routing/observability/router-telemetry-otel).

By [@abernix](https://github.com/abernix)