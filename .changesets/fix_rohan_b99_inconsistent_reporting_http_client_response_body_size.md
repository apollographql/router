### Report `http.client.response.body.size` and `http.server.response.body.size` consistently when `content-length` is absent or compression is used ([PR #8972](https://github.com/apollographql/router/pull/8972))

Reporting these metrics previously relied on either the `Content-Length` header or the `size_hint` of the body, which reports the uncompressed size. [OpenTelemetry semantic conventions](https://opentelemetry.io/docs/specs/semconv/http/http-metrics/#metric-httpclientrequestbodysize) recommend reporting the compressed size.

The router now consistently reports the compressed size when compression is used, even when `Content-Length` is absent, for:

- Router → client responses
- Subgraph → router responses
- Connector → router responses

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8972
