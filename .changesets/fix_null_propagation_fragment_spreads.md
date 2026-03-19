### Preserve null propagation when multiple fragments select the same non-null field ([PR #9032](https://github.com/apollographql/router/pull/9032))

When a query uses multiple fragment spreads on the same parent type and a subgraph response is missing a required non-null field on a union member, the router now correctly returns `null` for the affected field rather than a partial object like `{"__typename": "A"}`.

The GraphQL specification requires that a non-null violation propagates `null` upward to the nearest nullable parent. Previously, if one fragment correctly nullified a field, a subsequent fragment on the same parent could overwrite that `null` with a partial result — producing a spec-incorrect response. No changes to queries or configuration are required.

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/9032
