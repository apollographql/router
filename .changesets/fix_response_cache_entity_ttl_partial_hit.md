### Store response-cached entities under their own TTL on partial `_entities` hits ([PR #10422](https://github.com/apollographql/router/pull/10422))

When an `_entities` fetch mixed cache hits with misses, the response cache stored the entities it fetched from the subgraph with the `Cache-Control` merged across the whole fetch. That merge takes the shortest remaining lifetime, so a freshly fetched entity was stored with whatever time its oldest cached sibling had left instead of the lifetime the subgraph advertised. Because the shortened lifetime was stored, the next partial hit shortened it again. Over time, related entities converged on one shared expiry and were refetched from the subgraph all at once.

Fetched entities are now stored under the `Cache-Control` of the subgraph response they came from. The client-facing `Cache-Control` header is unchanged: it still never advertises more time than the oldest cached entity in the response has left.

By [@jeffutter](https://github.com/jeffutter) and [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/10422
