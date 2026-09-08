### JWT authentication now redacts validation errors by default ([Issue #2053](https://apollographql.atlassian.net/browse/ROUTER-2053))

`authentication.router.jwt.on_error` now defaults to `RedactedError` instead of `Error`. With the previous default, a caller sending a malformed or tampered token received the full validation detail verbatim—for example the byte offset where base64 decoding failed, the complete list of signing algorithms the router accepts, or the issuers and audiences configured on the JWKS. None of that is actionable for a legitimate client, and it gives an attacker a probing oracle for your authentication setup.

With the new default, failed JWT authentication is rejected with the same HTTP status codes as before, but the response body carries a generic `Authentication failed` message instead. The full detail is unaffected: it remains available in the `apollo::authentication::jwt_status` request context value (readable from telemetry selectors, Rhai, and coprocessors) and in the `authentication.jwt.failure_code` attribute on the `apollo.router.operations.authentication.jwt` metric.

To keep the previous behavior, set `on_error` explicitly:

```yaml title="router.yaml"
authentication:
  router:
    jwt:
      on_error: Error
```

By [@zachfetters](https://github.com/zachfetters) in https://github.com/apollographql/router/pull/10191
