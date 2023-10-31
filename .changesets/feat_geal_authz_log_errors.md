### add a flag to disable authorization error logs ([Issue #4077](https://github.com/apollographql/router/issues/4077) & [Issue #4116](https://github.com/apollographql/router/issues/4116))

Authorization errors need flexible reporting depending on the use case. They can now be configured as follows:

```yaml title="router.yaml"
authorization:
  preview_directives:
    errors:
      log: true # default: true
      response: "errors" # possible values: "errors" (default), "extensions", "disabled"
```

Logging can be disabled if platform operators do not want to see the logs polluted by common authorization errors. And errors in responses can be moved to extensions to avoid exceptions raised by the clients. Or they can be disabled entirely if the clients should not learn anything about the authorization errors.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4076 & https://github.com/apollographql/router/pull/4122