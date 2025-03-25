### Add `apollo.router.open_connections` metric `state` attribute rename ([PR #7091](https://github.com/apollographql/router/pull/7091))

The `state` attribute on `apollo.router.open_connections` has been renamed to `http.connection.state`.

This enables us to use the [Otel convention] (https://opentelemetry.io/docs/specs/semconv/attributes-registry/http/#http-connection-state) and provide better consistency for users.

Note that `idle` state is not yet supported.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/7091