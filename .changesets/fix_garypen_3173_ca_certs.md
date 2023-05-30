### Add ca-certificates to our docker image ([Issue #3173](https://github.com/apollographql/router/issues/3173))

We removed `curl` from our docker images to improve security, which meant that our implicit install of `ca-certificates` (as a dependency of `curl`) was no longer performed.

This fix manually installs the `ca-certificates` package which is required for the router to be able to process TLS requests.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3174