### Ability to ignore other auth prefixes in the JWT plugin

You can now choose whether to ignore other header prefixes with the JWT plugin. Many applications will use the format of `Authorization: <scheme> <token>` and this will enable the use of other schemes within the `Authorization` header. 

If the header prefix is an empty string, this option will be ignored. 

You can configure this, such as:

```yaml title="router.yaml"
authentication:
  router:
    jwt:
      header_name: authorization
      header_value_prefix: "Bearer"
      ignore_mismatched_prefix: true
```

In the above, the router will ignore `Authorization: Basic <token>`, but process requests with `Authorization: Bearer <token>` defined.

By [@lleadbet](https://github.com/lleadbet) in https://github.com/apollographql/router/pull/4718