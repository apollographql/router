### feat: add `redact_query_validation_errors` supergraph config option ([PR #8888](https://github.com/apollographql/router/pull/8888))

Adds a new configuration option in the `supergraph` section, `redact_query_validation_errors`.

This will result in any number of query validation errors being replaced with a single

```json
{
  "message": "invalid query",
  "extensions": {
    "code": "UNKNOWN_ERROR"
  }
}
```

By [@phryneas](https://github.com/phryneas) in https://github.com/apollographql/router/pull/8888
