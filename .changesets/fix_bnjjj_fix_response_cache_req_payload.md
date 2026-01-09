### Response cache: do not accept unknown fields in request payload for invalidation ([PR #8752](https://github.com/apollographql/router/pull/8752))

- **Response Cache**: Reject invalid invalidation requests with unknown fields
  - Returns HTTP 400 (Bad Request) when unknown fields are present in invalidation requests

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8752