### Add `redact_query_validation_errors` supergraph config option ([PR #8888](https://github.com/apollographql/router/pull/8888))

The new `redact_query_validation_errors` option in the `supergraph` configuration section replaces all query validation errors with a single generic error:

```json
{
  "message": "invalid query",
  "extensions": {
    "code": "UNKNOWN_ERROR"
  }
}
```

By [@phryneas](https://github.com/phryneas) in https://github.com/apollographql/router/pull/8888
