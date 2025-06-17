## Cooperative Cancellation for Query Planning

This release introduces cooperative cancellation support for query planning operations. This feature allows the router
to gracefully handle query planning timeouts and cancellations, improving resource utilization.
Metrics are emitted for cooperative cancellation:

- Records the "outcome" of query planning on the `apollo.router.query_planning.plan.duration` metric.
- Records the "outcome" of query planning on the `query_planning` span.

### Example

Configuring a timeout in Measure Mode:
```yaml
supergraph:
  query_planning:
    experimental_cooperative_cancellation:
      enabled: true
      mode: measure
      timeout_in_seconds: 1.0
```

By [@Velfi](https://github.com/Velfi) in https://github.com/apollographql/router/pull/7604
