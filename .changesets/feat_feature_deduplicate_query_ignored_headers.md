### Add deduplicate_query_ignored_headers option for traffic shaping ([PR #6129](https://github.com/apollographql/router/pull/6129))

When the Router is figuring out if a query can be deduplicated, it considers the entire request, including headers. This means that if you have two requests that have a different header value but are otherwise identical, they will not be deduplicated.

You can use the new `deduplicate_query_ignored_headers` option to provide a list of headers to remove from this deduplication calculation.

```yaml title="router.yaml"
traffic_shaping:
  all:
    deduplicate_query: true
    deduplicate_query_ignored_headers: ["x-my-header"]
```

By [@andrewmcgivery](https://github.com/andrewmcgivery) in https://github.com/apollographql/router/pull/6129
