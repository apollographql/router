### Add Extensions with_lock() to try and avoid timing issues ([PR #5360](https://github.com/apollographql/router/pull/5360))

It's easy to trip over issues when interacting with Extensions because we inadvertently hold locks for too long. This can be a source of bugs in the router and causes a lot of tests to be flaky.

with_lock() avoids this kind of problem by explicitly restricting the lifetime of the Extensions lock.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/5360