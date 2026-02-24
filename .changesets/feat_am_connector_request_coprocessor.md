### Coprocessor hook for connectors request/response stages ([PR #8869](https://github.com/apollographql/router/pull/8869))

You can now configure a coprocessor hook for the `ConnectorRequest` and `ConnectorResponse` stages of the Router lifecycle.

```
coprocessor:
  url: http://localhost:3007
  connector:
    all:
      request:
        uri: true
        headers: true
        body: true
        context: all
        service_name: true
      response:
        headers: true
        body: true
        context: all
        service_name: true
```

By [@andrewmcgivery](https://github.com/andrewmcgivery) in https://github.com/apollographql/router/pull/8869
