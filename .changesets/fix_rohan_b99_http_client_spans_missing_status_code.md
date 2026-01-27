### Add status code and error type attributes to `http_request` spans ([PR #8775](https://github.com/apollographql/router/pull/8775))

The router now always adds the `http.response.status_code` attribute to `http_request` spans (for example, for `router -> subgraph` requests). The router also conditionally adds `error.type` for non-success status codes.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8775
