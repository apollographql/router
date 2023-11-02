### add a flag to disable authorization error logs ([PR #4076](https://github.com/apollographql/router/pull/4076))

Authorization errors can be seen as common usage of the service when filtering fields from queries depending on the client's rights, so they might not warrant error logs to be analyzed by the router operators

Those logs can be disabled in the configuration:

```yaml
authorization:
  preview_directives:
    log_errors: true
```

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4076