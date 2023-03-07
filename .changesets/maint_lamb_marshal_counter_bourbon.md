### Improve CI time by removing test-binaries from build ([Issue #2625](https://github.com/apollographql/router/issues/2625))

We now have an experimental plugin called `broken` that is included in the router.
This removes the need to use `test-binaries` and avoids a full recompile of the router during integration testing.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2650
