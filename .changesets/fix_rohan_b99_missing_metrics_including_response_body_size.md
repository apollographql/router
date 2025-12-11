### Record `http.server.response.body.size` metric correctly ([PR #8697](https://github.com/apollographql/router/pull/8697))

Previously, the `http.server.response.body.size` metric wasn't recorded because the router attempted to read from the `Content-Length` header before it had been set. The router now uses the `size_hint` of the body if it's exact.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8697
