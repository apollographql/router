### GraphOS Enterprise: authorization directives ([PR #3397](https://github.com/apollographql/router/pull/3397), [PR #3662](https://github.com/apollographql/router/pull/3662))

We introduce two new directives, `requiresScopes` and `@authenticated`, that define authorization policies for fields and types in the supergraph schema.

They are defined as follows:

```graphql
scalar federation__Scope
directive @requiresScopes(scopes: [[federation__Scope!]!]!) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM

directive @authenticated on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
```

The implementation hooks into the request lifecycle at multiple steps:
- In query analysis, we extract the list of scopes necessary to authorize the query.
- In a supergraph plugin, we calculate the authorization status and put it in the request context:
    - for `@requiresScopes`, this is the intersection of the query's required scopes and the scopes provided in the request token
    - for `@authenticated`, it is `is_authenticated` or not
- In the query planning phase, we filter the query to remove unauthorized fields before proceeding with query planning.
- At the subgraph level, if query deduplication is active, the authorization status is used to group queries together.
- At the execution service level, the response is first formatted according to the filtered query, which removed any unauthorized information, then to the shape of the original query, which propagates nulls as needed.
- At the execution service level, errors are added to the response indicating which fields were removed because they were not authorized.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3397 https://github.com/apollographql/router/pull/3662