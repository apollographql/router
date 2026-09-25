### Add the `->withWarning` and `->withError` mapping methods for connectors

Connector mappings can now report a problem without failing the field. Both methods return their input unchanged and record a problem, so a mapping that recognizes a value it cannot vouch for can say so and still return the data. They differ in the severity they record and in who reads the result.

`->withWarning` declares a diagnostic addressed to the mapping author. It reaches the connectors debugger and the mapping-problems telemetry selectors, and never a client:

```graphql
@connect(
  http: { GET: "/v1/widgets/{$args.id}" }
  selection: """
  id
  availability: stock_code->match(
    ["A", "IN_STOCK"],
    ["B", "BACKORDERED"],
    [@, @->withWarning("Unrecognized stock code")]
  )
  """
)
```

`->withError`, on the other hand, declares an error addressed directly to the client:

```graphql
availability: stock_code ?? $("UNKNOWN")->withError({
  message: "Stock code was missing"
  extensions: { code: "INTERNAL_SERVER_ERROR", number: 210099 }
})
```
Both `->withError` and `->withWarning` can be constructed by passing the message as the argument:

Composition is also stricter for every mapping method, not only these two. A selection whose field shape is an error, which is what a method called with the wrong arguments produces, is now rejected at composition with the method's own diagnostic. Previously that diagnosis was computed and discarded, the selection type-checked, and the field silently produced nothing at request time. A subgraph carrying such a mapping composes today and will not after this change; the fix is the one the diagnostic names.

Several errors about one value are several calls. Both methods pass their input through, so the chain composes them:

```
@->withError("Code is unrecognized")->withError("Amount is negative")
```

To build a message out of prose and data, build the string:

```
@->withWarning(["Unrecognized stock code:", @.stock_code]->joinNotNull(" "))
```

A failed argument costs the message, never the value. If the argument produces nothing, the field still resolves with the value it had and two problems are reported: why the argument produced nothing, and that the message was never recorded. This matters most for `x ?? $(default)->withError(...)`, where deleting the value would destroy the default the author supplied. Use `??` inside the argument to spell an absence out in the text instead of losing the message to it.

Note that neither method can annotate a value that is not there. In `@.missing->withWarning("...")` the chain stops before the method runs, so nothing is recorded. Supply a value first, as in `@.missing ?? $(null)->withWarning("...")`.

Errors declared with `->withError` are reported in the response's `extensions`, under a `connectorErrors` array, with the author's `code` and `extensions` and a `path` naming the field they were declared at. Given an API response of `{ "id": "1", "stock_code": "C" }`, the client receives the value the API sent and the author's account of why it is suspect:

```json
{
  "data": { "widget": { "id": "1", "availability": "C" } },
  "extensions": {
    "connectorErrors": [
      {
        "message": "Unrecognized stock code",
        "path": ["widget", "availability"],
        "extensions": {
          "code": "CONNECTORS_MAPPING_ERROR",
          "service": "inventory",
          "connector": {
            "coordinate": "inventory:Widget.availability[0]",
            "selectionPath": "stock_code"
          }
        }
      }
    ]
  }
}
```

They are reported there rather than in `errors` because the field they describe resolved: [the GraphQL specification](https://spec.graphql.org/draft/#sec-Errors.Execution-Errors) requires that a response position at which an execution error was raised not appear in `data`, and returning the data is the point.

`extensions.connectorErrors` uses the message the error code declared in `->withError` by the author. It is a distinct thing from a connector's HTTP request failing, which surfaces as an ordinary GraphQL error in `errors` with a `CONNECTORS_FETCH` code.

Reporting is governed by [`include_subgraph_errors`](https://www.apollographql.com/docs/graphos/routing/observability/subgraph-error-inclusion), under the name of the subgraph the connector belongs to, since these messages are written by that subgraph's schema author and can interpolate data from the API's response. `include_subgraph_errors` configuration is off by default, meaning subgraph errors are redacted. Connector's declared errors will also be redacted. Set `include_subgraph_errors: { all: true }`, or `true` for the connector's subgraph, to have them reported. Declared errors for a fully redacted subgraph are omitted rather than replaced by a `Subgraph errors redacted` placeholder.
The connector's `service` and `connector.coordinate` extensions are preserved alongside the author's fields.

Both methods' messages appear in the connectors debugger, and in telemetry through the `connector_response_mapping_problems` selector, as all mapping problems do.

A declared error can be monitored under two different metrics using author's `code`:
* `apollo.router.graphql_error` under the author's `code`
* `apollo.router.operations.error` when `telemetry.apollo.errors.preview_extended_error_metrics` is enabled

It should be noted that using a wide variety of error codes will increase metric cardinality, so variability is advised to be kept at a minimum.

A warning is never counted as an error by either instrument.

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/10050 and [@dariuszkuc](https://github.com/dariuszkuc) in https://github.com/apollographql/router/pull/10160
