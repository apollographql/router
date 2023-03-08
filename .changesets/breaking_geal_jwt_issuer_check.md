### Add an issuer check after JWT signature verification ([Issue #2647](https://github.com/apollographql/router/issues/2647))

A JWKS URL can now be associated with an issuer in the YAML configuration. After verifying the JWT signature, if the issuer is configured in YAML, and there is an `iss` claim in the JWT, the router will check that they match, and reject the request if not.

Breaking:

The configuration changes from:

```yaml
Authentication:
  experimental:
    jwt:
      jwks_urls:
        - file:///path/to/jwks.json
        - http:///idp.dev/jwks.json
```

to:

```yaml
authentication:
  experimental:
    jwt:
      jwks:
        - url: file:///path/to/jwks.json
          issuer: "http://idp.local" # optional field
        - url: http:///idp.dev/jwks.json
          issuer: http://idp.dev # optional field
```

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2672