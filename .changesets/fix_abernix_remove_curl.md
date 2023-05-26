### Move `curl` dependency to separate layer in Docker image ([Issue #3144](https://github.com/apollographql/router/issues/3144))

We've moved `curl` out of the Docker image we publish.  The `curl` command is only used in the image we produce today for the sake of downloading dependencies.  It is never used after that, but we can move it to a separate layer to further remove it from the image.

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/3146