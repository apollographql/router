### Entity errors without an index in their path are no longer dropped by the caching plugins ([REG-2060](https://apollographql.atlassian.net/browse/REG-2060))

When `preview_entity_cache` or `response_cache` reassembles an `_entities` fetch, it replaces the subgraph's error list with the errors it could match to a fetched entity. An error is matched by the index in its path — `["_entities", 0, "name"]` — so errors reported without that index were matched to nothing and silently discarded. Clients saw nulled fields with no error explaining them, or, with `supergraph.enable_result_coercion_errors` on, only a `RESPONSE_VALIDATION_FAILED` "Missing field".

Not every subgraph framework emits the index: async-graphql resolves `_entities` without a context per list element and reports `["_entities", "name"]`, so every entity-fetch error from such a subgraph was lost whenever the response cache was enabled for it. The plain router, with caching off, has always handled these paths correctly.

Errors that cannot be tied to one fetched entity are now passed through untouched, which produces the same client-facing response as running with the cache disabled. Because the router cannot tell which entity such an error invalidates, **no entity from that fetch is cached** — a subgraph that returns pathless or index-less errors alongside otherwise cacheable entities will see those entities skipped rather than stored. The cache debugger reports this as a `SUBGRAPH_ERRORS` warning with `shouldStore: false` on the affected entries.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/PULL_NUMBER
