### Return 400 instead of 500 for request bodies that fail to decompress ([PR #10731](https://github.com/apollographql/router/pull/10731))

When a client sent a compressed request body that couldn't be decoded according to its `Content-Encoding` header, for example a truncated brotli stream or gzip data labelled `br`, the router responded with `500 Internal Server Error` and logged the decoder error (such as `reached unexpected EOF`) at `error` level.

The router now treats this as a client error. It responds with `400 Bad Request` and an `INVALID_GRAPHQL_REQUEST` error whose `details` extension describes the decompression failure:

```json
{"errors":[{"message":"Invalid GraphQL request","extensions":{"code":"INVALID_GRAPHQL_REQUEST","details":"failed to decompress the request body: reached unexpected EOF"}}]}
```

These rejections are now logged at `debug` level instead of `error`.

By [@cb372](https://github.com/cb372) in https://github.com/apollographql/router/pull/10731
