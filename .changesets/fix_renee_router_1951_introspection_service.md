### Resolve introspection queries before query planning ([PR #9756](https://github.com/apollographql/router/pull/9756))

GraphQL queries requesting the `__schema` or `__type` introspection fields are now executed inside the supergraph service. Previously, such queries were executed inside the query planner service. This change is visible in the spans reported by Router telemetry: introspection queries no longer emit a query planner span.

GraphQL queries requesting _only_ `__typename` are now treated the same way as any other (non-introspection) query. Previously, `__typename` could be resolved at several different locations in the Router, depending on if an alias was used or if it was requested along with other fields. Now, `__typename` is always resolved in the same way. This change is also visible in the spans reported by Router telemetry: queries that only request `__typename` will progress to the execution span instead of being short-circuited inside the query planner span.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/9756