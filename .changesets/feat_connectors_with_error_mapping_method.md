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

Errors declared this way are added to the GraphQL response's `errors` array, alongside the data, with the author's `code` and `extensions`. The connector's `service` and `connector.coordinate` extensions are preserved alongside the author's fields. Declared errors also continue to appear in the connectors debugger and telemetry, as all mapping messages do.

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/10050 and [@dariuszkuc](https://github.com/dariuszkuc) in https://github.com/apollographql/router/pull/10160
