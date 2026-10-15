### Report HTTP header limits in the `apollo.router.config.limits` usage metric ([PR #10478](https://github.com/apollographql/router/pull/10478))

The `apollo.router.config.limits` gauge now has three more attributes, each set when the matching `limits.router` option is configured: `opt.request.http1_max_headers` (`http1_max_request_headers`), `opt.request.http1_max_buf_size` (`http1_max_request_buf_size`) and `opt.request.http2_max_headers_list_bytes` (`http2_max_headers_list_bytes`).

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/10478
