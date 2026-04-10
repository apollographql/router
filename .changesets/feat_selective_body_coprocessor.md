### Add selective body field filtering for coprocessor responses ([Issue #5020](https://github.com/apollographql/router/issues/5020))

Adds the ability to selectively send only specific parts of GraphQL response bodies (data, errors, or extensions) to the coprocessor, rather than the entire response body. This reduces serialization/deserialization overhead and network payload size when the coprocessor only needs to inspect certain fields.

Previously, the `body` configuration was a boolean that sent either the entire response body or nothing. Now it supports selective field filtering:

```yaml
coprocessor:
  url: http://127.0.0.1:8081

  # Supergraph responses
  supergraph:
    response:
      body:
        data: false
        errors: true        # Only send errors
        extensions: true    # and extensions

  # Execution responses
  execution:
    response:
      body:
        data: true
        errors: false
        extensions: false   # Only send data

  # Subgraph responses
  subgraph:
    all:
      response:
        body:
          data: false
          errors: true      # Only send errors
          extensions: false
```

The boolean syntax (`body: true` or `body: false`) continues to work for backward compatibility. When using selective filtering, the coprocessor can only modify the fields that were sent to it; other fields are preserved from the original response.

This feature is available for the supergraph, execution, and subgraph response stages.

By [@zachfettersmoore](https://github.com/zachfettersmoore) in https://github.com/apollographql/router/issues/5020
