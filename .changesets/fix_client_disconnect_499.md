### Record client disconnects as 499 and malformed request bodies as 400 ([PR #PULL_NUMBER](https://github.com/apollographql/router/pull/PULL_NUMBER))

When a client closes its connection before the router has responded, the router now records the request with status `499` in `apollo.router.operations`, the `http.server.*` metrics and the router span. This applies to HTTP/1 and HTTP/2 clients, whether the client leaves while it is still sending the request body or while the router is working on the request, and whatever `supergraph.early_cancel` is set to. Previously these requests were recorded as `500`, or not recorded at all.

A request with a malformed HTTP/1 body, such as invalid chunked encoding, now gets a `400 Bad Request` response and is recorded as `400` instead of `500`.

Because requests that clients abandon are now counted, expect total request counts in these metrics to rise and 5xx rates to fall. With `supergraph.log_on_broken_pipe` enabled, the router now logs these disconnects whatever `early_cancel` is set to. You don't need to change your configuration.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/PULL_NUMBER
