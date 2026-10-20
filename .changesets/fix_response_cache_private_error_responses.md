### Error responses no longer turn off response caching for a query

With response caching enabled, the router keeps a list of subgraph queries that return private data. Without `private_id`, requests for a query on that list skip the cache. Before this fix, any response with `Cache-Control: private` put its query on the list, including error responses. Many frameworks send `private` on errors, so one failed request could turn off caching for that query, for every variable value, until the entry was evicted or the router restarted.

Now a response marks its query as private only if it succeeded and its `Cache-Control` header allows storing (no `no-store`, and a TTL above zero). A subgraph response succeeds when it has a 2xx status and no GraphQL errors. This applies to root fields and entities.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10437
