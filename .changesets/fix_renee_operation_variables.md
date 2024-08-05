### Fix GraphQL query directives validation bug ([PR #5753](https://github.com/apollographql/router/pull/5753))

GraphQL supports an obscure syntax, where a variable is used in a directive application on the same operation where the variable is declared.

The router used to reject queries like this, but now they are accepted:

```graphql
query GetSomething($var: Int!) @someDirective(argument: $var) {
  something
}
```

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/5753
