### Add `send_cache_control_header` config option to suppress `Cache-Control` on client responses ([PR #XXXX](https://github.com/apollographql/router/pull/XXXX))

The response cache plugin now supports a `send_cache_control_header` boolean config option (defaults to `true`). When set to `false`, the router omits the `Cache-Control` header from supergraph responses sent to clients, while all internal caching behavior — Redis storage, TTL enforcement, cache key computation, and the cache debugger — remains unchanged.

This is useful when the router sits behind a CDN or reverse proxy that manages its own caching headers, or when you want to prevent clients from caching responses locally while keeping server-side caching active.

```yaml
response_cache:
  enabled: true
  send_cache_control_header: false  # default: true
  subgraph:
    all:
      enabled: true
      redis:
        urls: ["redis://..."]
```

By [@ebylund](https://github.com/ebylund) in https://github.com/apollographql/router/pull/XXXX
