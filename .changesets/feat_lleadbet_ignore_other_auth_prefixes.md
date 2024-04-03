### Ignore other auth prefixes in the JWT plugin

If the router encounters an authorization header with a different prefix in the value than what it expects, it will now ignore it. If the router was configured without the `require_authentication` option or without the authorization directives, then some requests that came with a different header prefix that were rejected before will now go through the router. If those options were configured, then there will be no change in behaviour.

As an example, with a router configure like this:

```yaml title="router.yaml"
authentication:
  router:
    jwt:
      header_name: authorization
      header_value_prefix: "Bearer"
```

In the above, the router will ignore `Authorization: Basic <token>`, but process requests with `Authorization: Bearer <token>` defined.

By [@lleadbet](https://github.com/lleadbet) in https://github.com/apollographql/router/pull/4718