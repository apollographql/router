### `require_authentication` option to reject unauthenticated requests ([Issue #2866](https://github.com/apollographql/router/issues/2866))

While the authentication plugin validates queries with JWT, it does not reject unauthenticated requests, and leaves that to other layers. This allows co-processors to handle other authentication methods, and plugins at later layers to authorize the reqsuest or not. Typically, [this was done in rhai](https://www.apollographql.com/docs/router/configuration/authn-jwt#example-rejecting-unauthenticated-requests).

This now adds an option to the Router's YAML configuration to reject unauthenticated requests. It can be used as follows:

```yaml
authorization:
	require_authentication: true
```

The plugin will check for the presence of the `apollo_authentication::JWT::claims` key in the request context as proof that the request is authenticated.


By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3002