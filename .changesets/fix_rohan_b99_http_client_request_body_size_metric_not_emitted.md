### Ensure `http.client.request.body.size` metric is emitted ([PR #8712](https://github.com/apollographql/router/pull/8712))

The histogram for `http.client.request.body.size` was using the `SubgraphRequestHeader` selector, looking for `Content-Length` before it had been set in `on_request`, so `http.client.request.body.size` was not recorded. Instead, we now use the `on_response` handler, and store the body size in the request context extensions.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8712
