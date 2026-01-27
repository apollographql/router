### Reject invalidation requests with unknown fields ([PR #8752](https://github.com/apollographql/router/pull/8752))

The response cache invalidation endpoint now rejects request payloads that include unknown fields. When unknown fields are present, the router returns HTTP `400` (Bad Request).

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8752