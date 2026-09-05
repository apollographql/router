### Add the `->withError` mapping method for connectors

Connector mappings can now report a problem without failing the field. `->withError` returns its input unchanged and records an error, so a mapping that recognizes a value it cannot vouch for can say so and still return the data:

```graphql
@connect(
  http: { GET: "/v1/widgets/{$args.id}" }
  selection: """
  id
  availability: stock_code->match(
    ["A", $("IN_STOCK")],
    ["B", $("BACKORDERED")],
    [@, @->withError("Unrecognized stock code:", @)]
  )
  """
)
```

Any number of arguments is allowed, and they may be of any type. String arguments are interpolated as written, every other value is serialized as `->jsonStringify` would serialize it, and the parts are joined with single spaces into one message. Use `??` to supply a fallback where a path may be missing, since an argument that produces no value short-circuits the method rather than recording a partial message.

Errors declared this way are reported in the response's `extensions`, under a `connectorErrors` array, with the author's `code` and `extensions` and a `path` naming the field they were declared at. Given an API response of `{ "id": "1", "stock_code": "C" }` — a code matching no arm of the `->match` above — the client receives the value the API sent *and* the author's account of why it is suspect:

```json
{
  "data": { "widget": { "id": "1", "availability": "C" } },
  "extensions": {
    "connectorErrors": [
      {
        "message": "Unrecognized stock code: C",
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

They are reported there rather than in `errors` because the field they describe resolved: [the GraphQL specification](https://spec.graphql.org/draft/#sec-Errors.Execution-Errors) requires that a response position at which an execution error was raised not appear in `data`, and returning the data is the point of `->withError`.

Reporting is governed by [`include_subgraph_errors`](https://www.apollographql.com/docs/graphos/routing/observability/subgraph-error-inclusion), under the name of the subgraph the connector belongs to — these messages are written by that subgraph's schema author and can interpolate data from the API's response. **This includes the default**: with no `include_subgraph_errors` configuration, subgraph errors are redacted, and a connector's declared errors are omitted from the response along with them. Set `include_subgraph_errors: { all: true }`, or `true` for the connector's subgraph, to have them reported. A fully redacted subgraph's declared errors are omitted rather than replaced by a `Subgraph errors redacted` placeholder; short of full redaction, `redact_message` and the extension allow/deny lists apply exactly as they do to the `errors` array.

The connector's `service` and `connector.coordinate` extensions are preserved alongside the author's fields. Declared errors also continue to appear in the connectors debugger and telemetry, as all mapping messages do.

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/10050 and [@dariuszkuc](https://github.com/dariuszkuc) in https://github.com/apollographql/router/pull/10160
