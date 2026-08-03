### Add a `RedactedError` mode for JWT authentication errors

By default, when JWT authentication fails, the router returns a detailed error message describing the validation failure. Those messages can disclose details of your authentication setup to unauthenticated callers, including which signing algorithms the router accepts, the byte offsets at which token decoding failed, and the issuers and audiences the router is configured to trust.

Setting `authentication.router.jwt.on_error` to `RedactedError` rejects failed requests with the same status codes as `Error`, but replaces every message with a generic `Authentication failed`:

```yaml title="router.yaml"
authentication:
  router:
    jwt:
      jwks:
        - url: https://auth.example.com/.well-known/jwks.json
      on_error: RedactedError
```

The details of the failure are still available to you: the `apollo::authentication::jwt_status` context value carries the full message, code, and reason, and failures on the `apollo.router.operations.authentication.jwt` metric now carry an `authentication.jwt.failure_code` attribute (for example `CANNOT_DECODE_JWT` or `INVALID_AUDIENCE`) that you can use to break down why authentications fail.

The default remains `Error`, so this change doesn't affect existing deployments.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/TBD
