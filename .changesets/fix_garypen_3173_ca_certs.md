### Add `ca-certificates` to our Docker image ([Issue #3173](https://github.com/apollographql/router/issues/3173))

We removed `curl` from our Docker images to improve security, which meant that our implicit install of `ca-certificates` (as a dependency of `curl`) was no longer performed.

This fix reinstates the `ca-certificates` package explicitly, which is required for the router to be able to process TLS requests.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3174
