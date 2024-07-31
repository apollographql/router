### Fix GraphQL query directives validation bug

GraphQL supports an obscure syntax, where a variable is used in a directive application on the same operation where the variable is declared.

The router used to reject queries like this, but now they are accepted:

```graphql
query GetSomething($var: Int!) @someDirective(argument: $var) {
  something
}
```
