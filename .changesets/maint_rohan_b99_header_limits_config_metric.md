### Report HTTP header limit usage to Apollo ([PR #10478](https://github.com/apollographql/router/pull/10478))

The router now reports to Apollo whether these configuration options are used:

- `limits.router.http1_max_request_headers`
- `limits.router.http1_max_request_buf_size`
- `limits.router.http2_max_headers_list_bytes`

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/10478
