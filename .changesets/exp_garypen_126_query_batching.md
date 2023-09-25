### query batching prototype ([Issue #126](https://github.com/apollographql/router/issues/126))

An experimental implementation of query batching which adds support for client request batching to the Apollo Router.

If you’re using Apollo Client, you can leverage the in-built support for batching to reduce the number of individual requests sent to the Apollo Router.

Once [configured](https://www.apollographql.com/docs/react/api/link/apollo-link-batch-http/), Apollo Client will automatically combine multiple operations into a single HTTP request. The number of operations within a batch is client configurable, including the maximum number of operations in a batch and the maximum duration to wait for operations to accumulate before sending the batch request. 

The Apollo Router must be configured to receive batch requests, otherwise it rejects them. When processing a batch request, the router deserializes and processes each operation of a batch independently, and it responds to the client only after all operations of the batch have been completed.

```yaml
experimental_batching:
  enabled: true
  mode: batch_http_link
```

All operations within a batch will execute concurrently with respect to each other.

Do not attempt to use subscriptions or `@defer` queries within a batch as they are not supported.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3837
