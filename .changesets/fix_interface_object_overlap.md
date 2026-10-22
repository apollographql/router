### Plan overlapping interface fragments reached through interface objects

Query planning now handles a fragment on an interface that shares concrete implementations with an interface represented by `@interfaceObject`. Matching fields are preserved instead of failing with a missing `__typename` edge, and runtime typename continues to be obtained from an appropriate subgraph.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10292
