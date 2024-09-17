### Fix variout edge cases for `__typename` field ([PR #6009](https://github.com/apollographql/router/pull/6009))

The router now correctly handles the `__typename` field used on operation root types, even when the subgraph's root type has a name that differs from the supergraph's root type.

For example, in query like this:
```graphql
{
  ...RootFragment
}

fragment RootFragment on Query {
  __typename
  me {
    name
  }
}
```
Even if the subgraph's root type returns a `__typename` that differs from `Query`, the router will still use `Query` as the value of the `__typename` field.

This change also includes fixes for other edge cases related to the handling of `__typename` fields. For a detailed technical description of the edge cases that were fixed, please see [this description](https://github.com/apollographql/router/pull/6009#issue-2529717207).

By [@IvanGoncharov](https://github.com/IvanGoncharov) in https://github.com/apollographql/router/pull/6009