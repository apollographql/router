### detached coprocessor execution ([Issue #3297](https://github.com/apollographql/router/issues/3297))

This adds a new `detached` option at all coprocessor stages to allow the router to continue handling the request or response without waiting for the coprocessor to respond. This implies that the coprocessor response will not be used. This is targeted at use cases like logging and auditing, which do not need to block the router.

This can be set up in the configuration file, as follows:

```yaml title="router.yaml"
coprocessor:
  url: http://127.0.0.1:8081
  router:
    request:
      detached: true # optional. if set to true, the router will handle the request without waiting for the coprocessor to respond
      headers: true
```

This option works at all stages of coprocessor execution.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4902