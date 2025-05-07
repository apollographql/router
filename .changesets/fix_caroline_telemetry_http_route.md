### Spans should only include path in `http.route` ([PR #7390](https://github.com/apollographql/router/pull/7390))

Per the [OpenTelemetry spec](https://opentelemetry.io/docs/specs/semconv/attributes-registry/http/#http-route), the `http.route` should only include "the matched route, that is, the path template used in the format used by the respective server framework."

The router currently sends the full URI in `http.route`, which can be high cardinality (ie `/graphql?operation=one_of_many_values`). After this change, the router will only include the path (`/graphql`).

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7390
