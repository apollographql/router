### Add the `Content-Type` header to 413 and 414 responses from request size limits ([PR #9801](https://github.com/apollographql/router/pull/9801))

When a request exceeded `limits.router.http_max_request_bytes`, or its query string was too long, the router returned `413 Payload Too Large` or `414 URI Too Long` with a JSON error body but no `Content-Type` header. These responses now include `Content-Type: application/json`.

By [@marcelomartins](https://github.com/marcelomartins) in https://github.com/apollographql/router/pull/9801
