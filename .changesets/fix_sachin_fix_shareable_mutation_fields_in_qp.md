### Error when query planning a satisfiable @shareable mutation field ([PR #8352](https://github.com/apollographql/router/pull/8352))

When query planning a mutation operation that executes a `@shareable` mutation field at top-level, query planning may unexpectedly error due to attempting to generate a plan where that mutation field is called more than once across multiple subgraphs. Query planning has now been updated to avoid generating such plans.

By [@sachindshinde](https://github.com/sachindshinde) in https://github.com/apollographql/router/pull/8352