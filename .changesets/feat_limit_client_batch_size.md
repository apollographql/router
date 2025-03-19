### Add configuration option to limit maximum batch size ([PR #7005](https://github.com/apollographql/router/pull/7005))

Add an optional `maximum_size` parameter to the batching configuration.

* When specified, the router will reject requests which contain more than `maximum_size` queries in the client batch.
* When unspecified, the router performs no size checking (the current behavior).

If the number of queries provided exceeds the maximum batch size, the entire batch fails with error code 422 (
`Unprocessable Content`). For example:

```json
{
  "errors": [
    {
      "message": "Invalid GraphQL request",
      "extensions": {
        "details": "Batch limits exceeded: you provided a batch with 3 entries, but the configured maximum router batch size is 2",
        "code": "BATCH_LIMIT_EXCEEDED"
      }
    }
  ]
}
```

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7005
