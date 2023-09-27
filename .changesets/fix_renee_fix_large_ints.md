### Fix error response on large number types in query transformations ([PR #3820](https://github.com/apollographql/router/pull/3820))

This bug caused the router to reject operations where a large hardcoded integer was used as input for a Float field:

```graphql
# Schema
type Query {
    field(argument: Float): Int!
}
# Operation
{
    field(argument: 123456789123)
}
```

This number is now correctly interpreted as a `Float`. This bug only affected hardcoded numbers, not numbers provided through variables.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/3820