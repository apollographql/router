### Record 431 responses for oversized HTTP/2 header lists

Over HTTP/2, requests whose header list exceeded `limits.router.http2_max_headers_list_bytes` (16KiB by default) were rejected by the HTTP/2 library before they reached the router. The client got either a `431` with no headers or body, or a closed connection, and neither appeared in `http.server.request.duration`, `apollo.router.operations` or traces. HTTP/1.1 rejections have been recorded since v2.16.3; HTTP/2 rejections were not, so a cookie-heavy browser request just over the limit was invisible.

The router now rejects these requests itself. An HTTP/2 request over the limit gets a `431 Request Header Fields Too Large` with a GraphQL error body, in the same shape as the `413` and `414` size-limit errors, and it's recorded in metrics and the `router` span like any other response.

To make this possible, the router advertises a larger header-list size to HTTP/2 clients: the configured limit plus three times the limit, with at most 256KiB added (64KiB with the default limit). Header lists over that advertised size are still rejected by the HTTP/2 library and aren't recorded. See [Request Limits](https://www.apollographql.com/docs/graphos/routing/security/request-limits#http2_max_headers_list_bytes) for what a client gets at each size.

By [@rohan-b99](https://github.com/rohan-b99)
