### Prevent query planning errors for `@shareable` mutation fields ([PR #8352](https://github.com/apollographql/router/pull/8352))

Query planning a mutation operation that executes a `@shareable` mutation field at the top level may unexpectedly error when attempting to generate a plan where that mutation field is called more than once across multiple subgraphs. Query planning now avoids generating such plans.

By [@sachindshinde](https://github.com/sachindshinde) in https://github.com/apollographql/router/pull/8352