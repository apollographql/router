### Introduce per-origin CORS policies ([PR #7853](https://github.com/apollographql/router/pull/7853))

Configuration can now specify different Cross-Origin Resource Sharing (CORS) rules for different origins using the `cors.policies` key. See the [CORS documentation](https://www.apollographql.com/docs/graphos/routing/security/cors) for details.

```yaml
cors:
  policies:
    # The default CORS options work for Studio.
    - origins: ["https://studio.apollographql.com"]
    # Specific config for trusted origins
    - match_origins: ["^https://(dev|staging|www)?\\.my-app\\.(com|fr|tn)$"]
      allow_credentials: true
      allow_headers: ["content-type", "authorization", "x-web-version"]
    # Catch-all for untrusted origins
    - origins: ["*"]
      allow_credentials: false
      allow_headers: ["content-type"]
```

By [@Velfi](https://github.com/Velfi) in https://github.com/apollographql/router/pull/7853
