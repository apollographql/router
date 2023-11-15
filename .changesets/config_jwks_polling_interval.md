### Adds a `poll_interval` option to the JWKS endpoint in the Authentication plugin ([Issue #4185](https://github.com/apollographql/router/issues/4185))

In order to avoid rate limiting concerns on JWKS endpoints, this introduces a new `poll_interval` configuration option to set the interval per each JWKS URL. The configuration option takes in a human-readable duration (e.g. `60s` or `1minute 30s`). Example that sets interval to 30 seconds: 

```yml
authentication:
  router:
    jwt:
      jwks:
        - url: https://dev-zzp5enui.us.auth0.com/.well-known/jwks.json
          poll_interval: 30s
```

By [@lleadbet](https://github.com/lleadbet) in https://github.com/apollographql/router/pull/4212
