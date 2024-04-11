### Subgraph support for query batching ([Issue #2002](https://github.com/apollographql/router/issues/2002))

As an extension to the ongoing work to support [client-side query batching in the router](https://github.com/apollographql/router/issues/126), the router now supports batching of subgraph requests. Each subgraph batch request retains the same external format as a client batch request. This optimization reduces the number of round-trip requests from the router to subgraphs.

Also, batching in the router is now a generally available feature: the `experimental_batching` router configuration option has been deprecated and is replaced by the `batching` option.

Previously, the router preserved the concept of a batch until a `RouterRequest` finished processing. From that point, the router converted each batch request item into a separate `SupergraphRequest`, and the router planned and executed those requests concurrently within the router, then reassembled them into a batch of `RouterResponse` to return to the client. Now with the implementation in this release, the concept of a batch is extended so that batches are issued to configured subgraphs (all or named). Each batch request item is planned and executed separately, but the queries issued to subgraphs are optimally assembled into batches which observe the query constraints of the various batch items.

To configure subgraph batching, you can enable `batching.subgraph.all` for all subgraphs. You can also enable batching per subgraph with `batching.subgraph.subgraphs.*`. For example:

```yaml
batching:
  enabled: true
  mode: batch_http_link
  subgraph:
    # Enable batching on all subgraphs
    all:
      enabled: true
```

```yaml
batching:
  enabled: true
  mode: batch_http_link
  subgraph:
    # Disable batching on all subgraphs
    all:
      enabled: false
    # Configure(over-ride) batching support per subgraph
    subgraphs:
      subgraph_1:
        enabled: true
      subgraph_2:
        enabled: true
```

Note: `all` may be over-ridden by `subgraphs`. This applies in general for all router subgraph configuration options.

To learn more, see [query batching in Apollo docs](https://www.apollographql.com/docs/router/executing-operations/query-batching/).

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/4661
