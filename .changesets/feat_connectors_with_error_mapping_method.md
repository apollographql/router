### Add the `->withError` mapping method for connectors

Connector mappings can now report a problem without failing the field. `->withError` returns its input unchanged and records its arguments as a mapping error, so a mapping that recognizes a value it cannot vouch for can say so and still return the data:

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

Recorded messages travel the same path as any other mapping problem: they appear in the connectors debugger and are available to telemetry, with identical messages collapsed into a single problem carrying a count.

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/10050
