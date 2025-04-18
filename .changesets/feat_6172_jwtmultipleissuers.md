### Allow JWT authorization options to support multiple issuers ([Issue #6172](https://github.com/apollographql/router/issues/6172))

Allow JWT authorization options to support multiple issuers using the same JWKs.

**Configuration change**: any `issuer` defined on currently existing `authentication.router.jwt.jwks` needs to be 
migrated to an entry in the `issuers` list. For example:

Before:
```yaml
authentication:
  router:
    jwt:
      jwks:
        - url: https://dev-zzp5enui.us.auth0.com/.well-known/jwks.json
          issuer: https://issuer.one
```

After:
```yaml
authentication:
  router:
    jwt:
      jwks:
        - url: https://dev-zzp5enui.us.auth0.com/.well-known/jwks.json
          issuers: 
            - https://issuer.one
            - https://issuer.two
```

By [@theJC](https://github.com/theJC) in https://github.com/apollographql/router/pull/7170
