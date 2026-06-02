### Stabilize subgraph dedup hash against HeaderMap iteration order

The `SubgraphRequest::to_sha256` helper, used as the key for subscription dedup
and the dedup-cache fast path, iterated `http::HeaderMap` directly. `HeaderMap`
does not guarantee a stable iteration order across requests, so two logically
identical requests could produce different SHA-256 hashes and miss the dedup
cache. The previous implementation acknowledged this with a `// this assumes
headers are in the same order` comment but did not enforce it. Header pairs are
now sorted before being fed to the hasher, making the hash deterministic for a
given set of (name, value) entries regardless of insertion order.

This also fixes a macOS-only flake in
`integration::subscriptions::ws_passthrough::test_subscription_ws_passthrough_dedup`,
where header bucket ordering differed often enough to defeat dedup in practice.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9497
