### Add `connectors.expose_mapping_problems` to surface mapping problems (e.g. `->withError`) to clients

Mapping problems recorded during a connector's response mapping — including messages recorded by [`->withError`](https://github.com/apollographql/router/pull/10050) — have only ever been visible through the connectors debugger and telemetry. A field that resolved successfully but tripped a `->withError` tap produced no trace of that in the response: `data` was correct, but there was nothing in `errors` for a client (or a test) to see.

Setting `connectors.expose_mapping_problems: true` adds those problems to the response's client-facing `errors` array, alongside the still-resolved `data`:

```yaml
connectors:
  expose_mapping_problems: true
```

```graphql
idCheck: id->match(
  ["nonexistent-id", $("matched")],
  [@, @->withError("Item 6 test: unrecognized id", @)]
)
```

```json
{
  "data": { "me": { "id": "1", "idCheck": "1" } },
  "errors": [
    {
      "message": "Item 6 test: unrecognized id 1",
      "path": ["me", "idCheck"],
      "extensions": { "code": "CONNECTOR_MAPPING_PROBLEM", "count": 1 }
    }
  ]
}
```

The value is unchanged — this is the same partial-result contract `->withError` already had with the debugger, now optionally extended to the client. `count` reflects how many times an identical message was recorded (for example, once per matching element inside a `->map`), so a repeated tap over a large array produces one error rather than one per element.

The option is off by default: most existing mapping problems (a mistyped field, a `->match` with no arm taken) were never authored as messages for clients, so turning this on is an explicit choice about a specific connector's mappings, not a blanket behavior change.

By [@dcwalter](https://github.com/dcwalter) in https://github.com/apollographql/router/pull/TODO
