### Connectors can serve `Subscription` fields (connect/v0.5)

Under connect/v0.5, `@connect` works on `Subscription` root fields. The router reads the HTTP response as a stream, and each Server-Sent Event or JSON Lines event becomes one subscription payload, with `$` as that event. With `http.stream: "array"`, `$` is every event so far instead. A plain JSON response is a subscription of one payload. An event whose selection yields no value is not delivered, so a selection like `$.result { temp }` skips events that carry no result, and errors declared with `->withError` appear in each payload's `extensions.connectorErrors`.

The router's `subscription` settings apply, and the router reports adoption with the `apollo.router.schema.connectors.subscriptions` gauge.

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/10663
