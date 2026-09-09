### Add the `->withError` and `->withConnectorError` mapping methods for connectors

Connector mappings can now report a problem without failing the field. Both methods return their input unchanged and record an error, so a mapping that recognizes a value it cannot vouch for can say so and still return the data. They differ in who reads the result.

`->withError` records a diagnostic for the mapping author. It reaches the connectors debugger and telemetry, and never a client:

```graphql
@connect(
  http: { GET: "/v1/widgets/{$args.id}" }
  selection: """
  id
  availability: stock_code->match(
    ["A", $("IN_STOCK")],
    ["B", $("BACKORDERED")],
    [@, @->withError("Unrecognized stock code")]
  )
  """
)
```

`->withConnectorError` declares an error addressed to the client. Writing it is the statement that this text is fit to leave the router:

```graphql
availability: stock_code ?? $("UNKNOWN")->withConnectorError({
  message: "Stock code was missing"
  extensions: { code: "INTERNAL_SERVER_ERROR", number: 210099 }
})
```

Each method takes exactly one argument, and what that argument means is fixed by the method's name rather than by its shape.

For `->withError`, a string is the message as written and any other value is JSON-encoded into it. For `->withConnectorError`, a string is the error's `message` and an object is `{ message, extensions }` taken as written; anything else is a mistake reported at composition or at request time rather than coerced, so a client never receives an error whose message reads `42`.

Several errors about one value are several calls. Both methods pass their input through, so the chain composes them:

```
@->withConnectorError("Code is unrecognized")->withConnectorError("Amount is negative")
```

To build a message out of prose and data, build the string:

```
@->withError($->echo(["Unrecognized stock code:", @.stock_code])->joinNotNull(" "))
```

A failed argument costs the message, never the value. If the argument produces nothing, the field still resolves with the value it had and two problems are reported: why the argument produced nothing, and that the message was never recorded. This matters most for `x ?? $(default)->withConnectorError(...)`, where deleting the value would destroy the default the author supplied. Use `??` inside the argument to spell an absence out in the text instead of losing the message to it.

Note that neither method can annotate a value that is not there. In `@.missing->withError("...")` the chain stops before the method runs, so nothing is recorded. Supply a value first, as in `@.missing ?? $(null)->withError("...")`.

Errors declared with `->withConnectorError` are reported in the response's `extensions`, under a `connectorErrors` array, with the author's `code` and `extensions` and a `path` naming the field they were declared at. Given an API response of `{ "id": "1", "stock_code": "C" }`, the client receives the value the API sent and the author's account of why it is suspect:

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

The method's name follows the response: what an author writes as `->withConnectorError` arrives as an entry in `extensions.connectorErrors`. It is a distinct thing from a connector's HTTP request failing, which surfaces as an ordinary GraphQL error in `errors` with a `CONNECTORS_FETCH` code.

Reporting is governed by [`include_subgraph_errors`](https://www.apollographql.com/docs/graphos/routing/observability/subgraph-error-inclusion), under the name of the subgraph the connector belongs to, since these messages are written by that subgraph's schema author and can interpolate data from the API's response. **This includes the default**: with no `include_subgraph_errors` configuration, subgraph errors are redacted, and a connector's declared errors are omitted from the response along with them. Set `include_subgraph_errors: { all: true }`, or `true` for the connector's subgraph, to have them reported. A fully redacted subgraph's declared errors are omitted rather than replaced by a `Subgraph errors redacted` placeholder; short of full redaction, `redact_message` and the extension allow/deny lists apply exactly as they do to the `errors` array.

The connector's `service` and `connector.coordinate` extensions are preserved alongside the author's fields. Both methods' messages also appear in the connectors debugger and telemetry, as all mapping messages do.

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/10050 and [@dariuszkuc](https://github.com/dariuszkuc) in https://github.com/apollographql/router/pull/10160
