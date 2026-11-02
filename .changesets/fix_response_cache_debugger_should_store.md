### Cache debugger and private queries gauge report what the response cache did ([PR #10436](https://github.com/apollographql/router/pull/10436))

The cache debugger now reports `shouldStore: false` when the response cache did not store the data:

- requests for a query the router has marked as private when no `private_id` is configured, because those requests skip the cache;
- responses and entities with GraphQL errors, root-field responses with no data, and connector entities that came back null.

`shouldStore` is the cache's decision to store an entry. It does not confirm that the write to Redis succeeded.

The `apollo.router.response_cache.private_queries.lru.size` gauge now also counts queries marked as private by entity fetches, so its value can rise after upgrading.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10436
