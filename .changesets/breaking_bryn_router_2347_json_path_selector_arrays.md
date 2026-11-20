### JSON path telemetry selectors return an array for any path that can match several values

The `response_data`, `response_errors`, `subgraph_response_data` and `subgraph_response_errors` selectors used to return a single value when their JSON path matched one value and an array when it matched several, so the same attribute could be a string on one request and an array on the next.

The shape of the path now decides. A path made only of field names and single indexes, such as `$[0].extensions.code`, returns the matched value. Any other path, such as one with `[*]`, `.*`, `..`, a slice, a union or a filter, always returns an array, even when it matches one value. When nothing matches, the attribute is omitted and `default` applies, as before.

```yaml
# Before: "NOT_FOUND" for one error, ["NOT_FOUND", "FORBIDDEN"] for two
# After: ["NOT_FOUND"] for one error, ["NOT_FOUND", "FORBIDDEN"] for two
response_errors: "$[*].extensions.code"

# Unchanged: always "NOT_FOUND"
response_errors: "$[0].extensions.code"
```

For paths that can match several values, these values also change:

- `null` matches are left out. `$[*].x` over `[{"x": null}, {"x": "a"}]` used to return the JSON string `"[null,\"a\"]"` and now returns `["a"]`.
- If every match is `null`, the attribute is omitted and `default` applies. It used to be a string such as `"[null,null]"`.
- Matches of different types give an array of strings instead of one JSON string.
- A match that is itself an array or object is written as one JSON string in the array. `$.*` over `{"items": [{"id": 1}, {"id": 2}]}` used to return `["{\"id\":1}", "{\"id\":2}"]` and now returns `["[{\"id\":1},{\"id\":2}]"]`.
- `response_errors_field` adds each non-null match from each error to its array, instead of writing an error with several matches as one JSON string.

If a dashboard or alert relies on one of these attributes being a single value, change its path to select one value, such as `$[0].extensions.code`.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10853
