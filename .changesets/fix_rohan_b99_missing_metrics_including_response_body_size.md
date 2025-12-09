### Ensure `http.server.response.body.size` metric is recorded ([PR #8697](https://github.com/apollographql/router/pull/8697))

Previously, the `http.server.response.body.size` metric was not recorded as we attempted to read from the `Content-Length` header of the response before it had been set. We now use the `size_hint` of the body if it is exact.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8697
