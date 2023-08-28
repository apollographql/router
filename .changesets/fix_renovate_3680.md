### Fix GraphQL block comment parser regression ([Issue #3680](https://github.com/apollographql/router/issues/3680))

In 1.28.0, the GraphQL parser falsely errored out on backslashes in block comments, such as:
```graphql
"""
A regex: '/\W/'
A path: PHP\Namespace\Class
"""
```

This now parses again.
