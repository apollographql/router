### Store each cached entity under its own TTL instead of the `_entities` batch minimum

Response cache entries created by a partial `_entities` hit were stored with the `Cache-Control` merged across the whole fetch, rather than the one the subgraph sent for the entities that were actually fetched. Because the merge takes the shortest remaining lifetime, an entity the subgraph had just declared good for its full TTL was written with whatever little time an unrelated sibling in the same fetch had left. That value was then persisted and inherited again on the next partial hit, so TTLs ratcheted down and entities collapsed into synchronized expiry cohorts that expired and refilled together.

The router now bases store decisions - TTL, storability, and privacy - on the cache-control of the response the entities came from. The client-facing `Cache-Control` header still reflects the merge across the whole fetch, which is unchanged and correct.

By [@jeffutter](https://github.com/jeffutter) in https://github.com/apollographql/router/pull/0000
