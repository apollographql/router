### Fix cross-section hash collision in subgraph `Request::to_sha256` ([Issue/PR #9497](https://github.com/apollographql/router/pull/9497))

The subgraph dedup hash concatenated its sections (headers, claim, operation_name, query, variables, extensions) with no domain separator. An empty section followed by a populated one fed the hasher the same bytes as the populated section followed by an empty one, so a request with `variables: {"k": "1"}, extensions: {}` produced the same SHA-256 as a request with `variables: {}, extensions: {"k": "1"}`. Because this hash drives the subgraph dedup cache and subscription dedup keying, the cache could serve one request's response back to a semantically distinct request.

Tag each section with a two-byte sentinel (`\0H`, `\0C`, `\0O`, `\0Q`, `\0V`, `\0E`) before its bytes, so cross-section collisions are no longer possible. Added two regression tests covering the `variables` ↔ `extensions` swap and the `operation_name` ↔ `query` concatenation collision.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9497
