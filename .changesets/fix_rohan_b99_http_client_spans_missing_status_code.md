### fix: add http.response.status_code and error.type attributes to http_request spans ([PR #8775](https://github.com/apollographql/router/pull/8775))

The `http.response.status_code` attribute is now always added to http_request spans (e.g. for router -> subgraph requests), and `error.type` is conditionally added for non-success status codes.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8775
