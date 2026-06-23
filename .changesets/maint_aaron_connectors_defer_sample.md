### Fix Linux flake in samples::/enterprise/connectors-defer

The `/enterprise/connectors-defer` samples test was intermittently failing on Linux CI with:

```
expected: [{"data":{"m":{"f":"1"}},"hasNext":true},{"hasNext":false,"incremental":[...],"path":["m"]}]
received: [{"data":{"m":{"f":"1"}},"hasNext":true},{"hasNext":true,"incremental":[...],"path":["m"]},{"data":null,"hasNext":false}]
```

This is the well-known two-shape framing of deferred multipart responses (see `filter_stream` race in `execution/service.rs`): both forms are spec-compliant, and which one the router emits depends on whether the channel disconnects before or after the final `try_recv`. PR #9263 introduced a `deferred_responses_equivalent` helper to bridge the two shapes, but its index-based fast-path comparison was fragile enough that this exact case still slipped through in CI (3 occurrences in the last 14 days, most recent CircleCI job 377016 on 2026-05-21).

The fix replaces the indexing logic with a small `collapse_terminator` normalizer that, when an array ends in `{ data: null, hasNext: false }` preceded by a chunk with `hasNext: true`, drops the terminator and flips the preceding chunk's `hasNext` to `false`. The equivalence check then becomes plain equality of the two normalized forms — symmetric, free of off-by-one risk, and trivially correct for fast-path inputs (which pass through unchanged). No router behaviour changes; this is a test-harness fix only.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9497
