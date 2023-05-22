### Restore HTTP payload size limit, make it configurable ([Issue #2000](https://github.com/apollographql/router/issues/2000))

Early versions of Apollo Router used to rely on a part of the Axum web framework
that imposed a 2 MB limit on the size of the HTTP request body.
Version 1.7 changed to read the body directly, unintentionally removing this limit.

The limit is now restored to help protect against unbounded memory usage, but is now configurable:

```yaml
preview_operation_limits:
  http_max_request_bytes: 2000000 # Default value: 2 MB
```

This limit is checked while reading from the network, before JSON parsing.
Both the GraphQL document and associated variables count toward it.

By [@SimonSapin](https://github.com/SimonSapin in https://github.com/apollographql/router/pull/3130
