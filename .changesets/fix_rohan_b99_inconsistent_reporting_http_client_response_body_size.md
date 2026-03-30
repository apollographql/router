### Report http.client.response.body.size and http.server.response.body.size consistently if content-length is not present or compression is used ([PR #8972](https://github.com/apollographql/router/pull/8972))

Currently reporting these metrics relies on either the Content-Length header, or the size_hint of the body (which will report uncompressed size). Semantic conventions recommends this should be the compressed size (https://opentelemetry.io/docs/specs/semconv/http/http-metrics/#metric-httpclientrequestbodysize)
This PR should consistently report the compressed size if compression is used, even if Content-Length is not present, for:
- Router -> Client responses
- Subgraph -> Router responses
- Connector -> Router responses

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8972