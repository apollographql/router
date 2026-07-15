### Fix missing `Content-Type` header on 413 responses when `http_max_request_bytes` limit is exceeded ([PR #9801](https://github.com/apollographql/router/pull/9801))

When a request exceeded the `limits.router.http_max_request_bytes` threshold, the Router correctly returned a `413 Payload Too Large` response with a JSON error body, but omitted the `Content-Type: application/json` response header — in violation of the HTTP spec.

This was caused by the request being rejected at the HTTP layer before reaching the normal GraphQL response pipeline, which is where `Content-Type` is normally set for other error types. The fix adds the header explicitly in the `BodyLimitError::into_response` path, matching the pattern already used for other error responses.

By [@marcelomartins](https://github.com/marcelomartins) in https://github.com/apollographql/router/pull/XXXX