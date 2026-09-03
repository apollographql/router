### Add the `->withError` mapping method for connectors

Connector mappings can now report a problem without failing the field. `->withError` returns its input unchanged and records an error, so a mapping that recognizes a value it cannot vouch for can say so and still return the data:

```graphql
@connect(
  http: { GET: "/v1/accounts/{$args.id}" }
  selection: """
  id
  status: type_code->match(
    ["2", $("VAN")],
    [@, @->withError("Unrecognized type code:", @, "for", $args.id)]
  )
  """
)
```

Any number of arguments is allowed, and they may be of any type. String arguments are interpolated as written, every other value is serialized as `->jsonStringify` would serialize it, and the parts are joined with single spaces into one message. Use `??` to supply a fallback where a path may be missing, since an argument that produces no value short-circuits the method rather than recording a partial message.

#### Structured errors that reach the client

A single object argument carrying a `message` field declares the error's parts directly, so it can carry an error code and structured fields instead of only a sentence. Combined with `??`, this resolves a required field with a default *and* reports the defect:

```graphql
selection: """
requiredField: $response.requiredField ?? $("<missing>")->withError({
  message: "Field 'requiredField' was not found"
  extensions: { code: "INTERNAL_SERVER_ERROR", number: 210099 }
})
"""
```

`??` short-circuits, so the `->withError` runs only when the left side produces nothing — a field that resolves normally reports nothing, and the failed path's own diagnostic is not recorded alongside the declared message.

Pick the operator to match what counts as missing. `??` treats an explicit `null` the same as an absent field, so both take the fallback; `?!` fills in only for an absent field and lets a real `null` through:

| `$response.field` | `?? $("<missing>")->withError(…)` | `?! $("<missing>")->withError(…)` |
| --- | --- | --- |
| absent | `"<missing>"`, error recorded | `"<missing>"`, error recorded |
| `null` | `"<missing>"`, error recorded | `null`, no error |

Use `?!` when "the API omitted this field" has to read differently from "the API sent null".

Errors declared this way are added to the GraphQL response's `errors` array, alongside the data, with the author's `code` and `extensions`. The connector's `service` and `connector.coordinate` extensions are preserved alongside the author's fields. Declared errors also continue to appear in the connectors debugger and telemetry, as all mapping messages do.

Like every mapping method, `->withError` behaves the same at every connect spec version; writing it is itself the opt-in.

Malformed calls are now rejected at composition rather than failing at runtime. A mapping method whose arguments are wrong produces an error shape — for example `Method ->withError requires at least one argument` — and composition previously computed that diagnosis and discarded it, accepting the field as an ordinary scalar. Such a selection now fails with an `INVALID_SELECTION` error naming the field and the reason. This applies to every mapping method, not only `->withError`, so a schema that called any method incorrectly and silently produced nothing at runtime now reports the mistake when it is published.

Two things worth knowing about the reported errors:

- **`path` names the field the mapping writes**, descending from the connector's response path into the mapping's output — `balance` in `balance: amount->withError(...)`, not `amount`. One gap remains: a client that *aliases* a field sees the alias in its response, while the error path carries the schema field name. The mapping-side path (where the mapping was reading in the API's response) is reported under `extensions.connector.selectionPath`.
- **Every declared error is reported by default**, the same way the router passes through every subgraph error. A `->withError` inside a `->map` records one error per element, so a mapping over a large API response can contribute an error per row; unlike mapping problems, declared errors are not collapsed by message, since each carries its own path. The new `limits.connector.max_mapping_errors` option bounds this when you need it to.

Messages that are *not* declared by `->withError` — the mapping language's own diagnostics, like a path that found nothing — continue to travel only to the connectors debugger and telemetry, and are never sent to clients. The coalescing operators drop those diagnostics when a fallback succeeds, so a defaulted field reports only the message its author declared, but they no longer discard the declared message itself: `field ?? $(null)->withError("...")` records its error rather than silently doing nothing.

Error detail can be varied by environment using `$config` or `$env`, which are in scope for response mappings:

```graphql
selection: """
field: $response.field ?? $("<unavailable>")->withError({
  message: $config.verboseErrors->match([true, $.detail], [@, "An error occurred"])
  extensions: { code: "INTERNAL_SERVER_ERROR" }
})
"""
```

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/10050 and [@dariuszkuc](https://github.com/dariuszkuc) in https://github.com/apollographql/router/pull/PR_NUMBER
