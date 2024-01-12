### Add options to set username and password separately for Redis ([Issue #4346](https://github.com/apollographql/router/issues/4346))

Example of configuration:

```yaml title="router.yaml"
supergraph:
  query_planning:
    experimental_cache:
      redis: #highlight-line
        urls: ["redis://..."] #highlight-line
        username: admin/123 # Optional, can be part of the urls directly, mainly useful if you have special character like '/' in your password that doesn't work in url. This field takes precedence over the username in the URL
        password: admin # Optional, can be part of the urls directly, mainly useful if you have special character like '/' in your password that doesn't work in url. This field takes precedence over the password in the URL
        timeout: 5ms # Optional, by default: 2ms
        ttl: 24h # Optional, by default no expiration
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/4453