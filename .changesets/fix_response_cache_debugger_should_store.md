### The response cache debugger and private queries gauge report accurately

The cache debugger reports `shouldStore: false` for requests that bypass the cache and for responses or entities excluded from storage because of GraphQL errors. Cache hits keep their existing value. This field reflects the cache's storage decision, not confirmation that a Redis write succeeded.

The `apollo.router.response_cache.private_queries.lru.size` gauge includes queries added by subgraph entity fetches, as well as root-field and connector fetches.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/PULL_NUMBER
