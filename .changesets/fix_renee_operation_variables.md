### Fix GraphQL query directives validation bug ([PR #5753](https://github.com/apollographql/router/pull/5753))

The router now supports GraphQL queries where a variable is used in a directive on the same operation where the variable is declared. 

For example, the following query both declares and uses `$var`: 

```graphql
query GetSomething($var: Int!) @someDirective(argument: $var) {
  something
}
```

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/5753
