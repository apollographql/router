### set DEBUG_IMAGE=true when building debug docker image ([Issue #3249](https://github.com/apollographql/router/issues/3249))

Router debug docker images were designed to make use of heaptrack for debugging memory issues. However, this functionality was broken when we changed to multi-architecture docker image builds.

This restores the heaptrack functionality to our debug docker images.

fixes: #3249

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3250