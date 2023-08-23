### GraphOS Enterprise: authorization directives ([PR #3397](https://github.com/apollographql/router/pull/3397))

We introduce two new directives, `@authenticated` and `requiresScopes`, that define authorization policies for field and types in the supergraph schema.

They are defined as follows:

```graphql
directive @authenticated on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM

scalar federation__Scope
directive @requiresScopes(scopes: [[federation__Scope!]!]!) on OBJECT | FIELD_DEFINITION | INTERFACE | SCALAR | ENUM
```

They are implemented by hooking the request lifecycle at multiple steps:
- in query analysis, we extract from the query the list of scopes that would be relevant to authorize the query
- in a supergraph plugin, we calculate the authorization status and put it in the context: `is_authenticated` for `@authenticated`, and the intersection of the query's required scopes and the scopes provided in the token, for `@requiresScopes`
- in the query planning phase, we filter the query to remove the fields that are not authorized, then the filtered query goes through query planning
- at the subgraph level, if query deduplication is active, the authorization status is used to group queries together
- at the execution service level, the response is formatted according to the filtered query first, which will remove any unauthorized information, then to the shape of the original query, which will propagate nulls as needed
- at the execution service level, errors are added to the response indicating which fields were removed because they were not authorized

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3397