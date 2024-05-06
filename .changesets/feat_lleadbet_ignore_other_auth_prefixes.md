### Ability to ignore auth prefixes in the JWT plugin

The router now supports a configuration to ignore header prefixes with the JWT plugin. Given that many application headers use the format of `Authorization: <scheme> <token>`, this option enables the router to process requests for specific schemes within the `Authorization` header while ignoring others.

For example, you can configure the router to process requests with `Authorization: Bearer <token>` defined while ignoring others such as `Authorization: Basic <token>`: 

```yaml title="router.yaml"
authentication:
  router:
    jwt:
      header_name: authorization
      header_value_prefix: "Bearer"
      ignore_mismatched_prefix: true
```

If the header prefix is an empty string, this option is ignored.

By [@lleadbet](https://github.com/lleadbet) in https://github.com/apollographql/router/pull/4718