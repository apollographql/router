### Update router-bridge@0.6.4+v2.9.3 ([PR #6161](https://github.com/apollographql/router/pull/6161))

Updates to latest router-bridge and federation version. This federation version:
- Fixes a query planning bug where operation variables for a subgraph query wouldn't match what's used in that query.
- Fixes a query planning bug where directives applied to `__typename` may be omitted in the subgraph query.
- Fixes a query planning inefficiency where some redundant subgraph queries were not removed.
- Fixes a query planning inefficiency where some redundant inline fragments in `@key`/`@requires` selection sets were not optimized away.
- Fixes a query planning inefficiency where unnecessary subgraph jumps were being added when using `@context`/`@fromContext`.

By [@sachindshinde](https://github.com/sachindshinde) in https://github.com/apollographql/router/pull/6161