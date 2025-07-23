### Support retry configuration for JWKS URL fetching ([Issue #TBD](https://github.com/apollographql/router/issues/TBD))

The router now supports configurable retry logic for JWKS (JSON Web Key Set) URL fetching in JWT authentication. Previously, failed JWKS requests would only retry at the next polling interval, potentially causing authentication failures during transient network issues.

```yaml
authentication:
  router:
    jwt:
      jwks:
        - url: "https://example.com/.well-known/jwks.json"
          retry:
            attempts: 3
            backoff:
              initial: 100ms
              max: 5s
              multiplier: 2.0
```

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/TBD
