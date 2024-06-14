### Add support of response_context selector when it's in error ([PR #5288](https://github.com/apollographql/router/pull/5288))

Provides the ability to configure custom instruments. For example:

```yaml
http.server.request.timeout:
  type: counter
  value: unit
  description: "request in timeout"
  unit: request
  attributes:
    graphql.operation.name:
      response_context: operation_name
  condition:
    eq:
    - "request timed out"
    - error: reason
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5288