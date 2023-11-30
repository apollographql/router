### Decrease default GraphQL parser recursion limit to 500 ([PR #4205](https://github.com/apollographql/router/pull/4205))

The Apollo Router's GraphQL parser uses recursion for nested selection sets, list values, or object values. The nesting level is limited to protect against stack overflow.

Previously the default limit was 4096. That limit has been decreased to 500 in this release.

You can change the limit (or backport the new default to older router versions) in YAML configuration:

```yaml
limits:
  parser_max_recursion: 700
```

> Note: deeply nested selection sets often cause deeply nested response data. When handling a response from a subgraph, the JSON parser has its own recursion limit of 128 nesting levels. That limit is not configurable.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/4205
