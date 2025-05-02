### Support non-JSON and JSON-like content types for connectors ([PR #7380](https://github.com/apollographql/router/pull/7380))

Connectors now inspect the `content-type` header of responses to determine how they should treat the response. This allows more flexibility as prior to this change, all responses were treated as JSON which would lead to errors on non-json responses.

The behavior is as follows:

- If `content-type` ends with `/json` (like `application/json`) OR `+json` (like `application/vnd.foo+json`): Content is parsed as JSON
- If no `content-type` header is provided: Content is assumed to be JSON and therefore parsed as JSON
- If content is `text/plain`, content will be treated as a UTF-8 `string`. Content can be accessed in `selection` mapping via `$` variable.
- If `content-type` is any other value, it will be treated as a JSON `null`

If deserialization fails, an error message of `Response deserialization failed` with a error code of `CONNECTOR_DESERIALIZE` will be returned:

```json
"errors": [
    {
        "message": "Response deserialization failed",
        "extensions": {
            "code": "CONNECTOR_DESERIALIZE"
        }
    }
]
```

By [@andrewmcgivery](https://github.com/andrewmcgivery) in https://github.com/apollographql/router/pull/7380
