### Subgraph support for query batching ([Issue #2002](https://github.com/apollographql/router/issues/2002))

This project is an extension of the existing work to support [client side batching in the router](https://github.com/apollographql/router/issues/126). The current implementation is experimental and is publicly [documented](https://www.apollographql.com/docs/router/executing-operations/query-batching/).

Currently the concept of a batch is preserved until the end of the `RouterRequest` processing. At this point, we convert each batch request item into a separate `SupergraphRequest`. These are then planned and executed concurrently within the router and re-assembled into a batch when they complete. It's important to note that, with this implementation, the concept of a batch, from the perspective of an executing router, now disappears and each request is planned and executed separately.

This extension will modify the router so that the concept of a batch is preserved, at least outwardly, so that multiple subgraph requests are "batched" (in exactly the same format as a client batch request) for onward transmission to subgraphs. The goal of this work is to provide an optimisation by reducing the number of round-trips to a subgraph from the router.

Illustrative configuration.

```yaml
batching:
  enabled: true
  mode: batch_http_link
  subgraph:
    all:
      enabled: true
    subgraphs:
      subgraph_1:
        enabled: true
      subgraph_2:
        enabled:true
````

As with other router subgraph configuration options, `all` and `subgraphs` are mutually exclusive.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/4661