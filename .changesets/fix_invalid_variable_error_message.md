### Improve error message for invalid variables  ([Issue #2984](https://github.com/apollographql/router/issues/2984))

Example:

```diff
-invalid type for variable: 'x'
+invalid input value at x.coordinates[0].longitude: found JSON null for GraphQL Float!
```

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/7567
