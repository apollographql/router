### Clarify OpenTelemetry "Recommended" attributes guidance in telemetry documentation

Update router telemetry documentation to clarify that OpenTelemetry's "Recommended" attributes from their [development-status GraphQL semantic conventions](https://opentelemetry.io/docs/specs/semconv/graphql/graphql-spans/) are experimental and still evolving. Apollo recommends using `required` attributes instead of `recommended` due to high cardinality, security, and performance risks with attributes like `graphql.document`.

Learn more in [Router Telemetry](https://www.apollographql.com/docs/graphos/routing/observability/router-telemetry-otel).

By [@abernix](https://github.com/abernix)