### Fix fragment usage with `@interfaceObject` ([Issue #3855](https://github.com/apollographql/router/issues/3855))

When requesting `__typename` under a fragment under an interface, from a subgraph adding fields to that interface with the `@interfaceObject` directive, the router was returning the interface name instead of the concrete type name. This is now fixed at the query planner level.

By [@geal](https://github.com/geal) in https://github.com/apollographql/router/pull/4363