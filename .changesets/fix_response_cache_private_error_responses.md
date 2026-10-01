### Error responses no longer turn off response caching for a query

With response caching enabled, the router remembers which subgraph queries return private data, so it can handle them separately from shared cache entries. Before this fix, any response with `Cache-Control: private` put its query on that list, including error responses. Many frameworks send `private` on errors, so without `private_id`, one failed request could turn off caching for that query, for every variable value, until the router restarted.

Now a response marks its query as private only if it succeeded and its `Cache-Control` header allows storing (no `no-store`, and a TTL above zero). A subgraph response succeeds when it has a 2xx status and no GraphQL errors. A connector response succeeds when the connector treats it as a success (by default a 2xx status, or whatever its `isSuccess` setting accepts) and its mapping declares no errors. This applies to root fields and entities.

If a connector's mapping declares an error with `->withError` in an entity response, the router now caches none of the entities in that response, whether or not `include_subgraph_errors` shows the error to the client. Before, it could cache the entity that had the error, or skip a different one.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/PULL_NUMBER
