### `null` extensions incorrectly disallowed on request ([Issue #4856](https://github.com/apollographql/router/issues/4856))

The [graphql over http spec](https://graphql.github.io/graphql-over-http/draft/#sel-EALFPCCBCEtC37P) mandates `null` is allowed for request extensions.

We were previously rejecting such payloads, but now we will allow them. For example:

```json
{
  "query": "{ topProducts { upc name reviews { id product { name } author { id name } } } }",
  "variables": {
    "date": "2022-01-01T00:00:00+00:00"
  },
  "extensions": null
}
```

Fixes #4856

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/4865
