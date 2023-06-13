### Restore missing debug tools in "debug" Docker images ([Issue #3249](https://github.com/apollographql/router/issues/3249))

Debug Docker images were designed to make use of `heaptrack` for debugging memory issues. However, this functionality was inadvertently removed when we changed to multi-architecture Docker image builds.

This restores the heaptrack functionality to our debug docker images.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3250