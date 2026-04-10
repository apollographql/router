### Fix connector handling of 204 responses without `Content-Length` header

Connectors now correctly handle HTTP 204 (No Content) responses from spec-compliant servers that do not include a `Content-Length` header.

Previously, empty body detection relied on the presence of a `Content-Length: 0` header. Since the HTTP spec explicitly forbids including this header in 204 responses, connectors would fail to recognize empty bodies from compliant servers. The fix checks
`body.is_empty()` directly, with `Content-Length: 0` kept as a fallback for non-compliant servers.

By [@apollo-mateuswgoettems](https://github.com/apollo-mateuswgoettems) in https://github.com/apollographql/router/pull/9141