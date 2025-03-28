### Improve Error Message for Invalid JWT Header Values ([PR #7121](https://github.com/apollographql/router/pull/7121))

Enhanced parsing error messages for JWT Authorization header values now provide developers with clear, actionable feedback while ensuring that no sensitive data is exposed.

Examples of the updated error messages:
```diff
-         Header Value: '<invalid value>' is not correctly formatted. prefix should be 'Bearer'
+         Value of 'authorization' JWT header should be prefixed with 'Bearer'
```

```diff
-         Header Value: 'Bearer' is not correctly formatted. Missing JWT
+         Value of 'authorization' JWT header has only 'Bearer' prefix but no JWT token
```

By [@IvanGoncharov](https://github.com/IvanGoncharov) in https://github.com/apollographql/router/pull/7121
