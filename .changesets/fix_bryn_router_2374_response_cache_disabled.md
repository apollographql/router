### `response_cache.enabled: false` turns off the client `Cache-Control` header ([PR #10856](https://github.com/apollographql/router/pull/10856))

With `response_cache: { enabled: false }`, the router didn't cache subgraph responses but still changed client responses. It merged each subgraph's and connector's `Cache-Control` header into the client response's `Cache-Control`, added the CDN invalidation header when `cdn_invalidation.enabled` was true, and added the cache debugger extension when `debug` was true. Now `enabled: false` turns all of this off, and client responses are the same as with no `response_cache` section.

If your clients or CDN rely on the merged `Cache-Control` header from a router with response caching disabled, they no longer receive it.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10856
