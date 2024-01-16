### update router-bridge to v0.5.14+v2.6.3 ([PR #4468](https://github.com/apollographql/router/pull/4468))

This federation update contains query planning fixes for:
* invalid typename used when calling into a subgraph that uses `@interfaceObject`
* performance issue when generating planning paths for union members that use `@requires`

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4468