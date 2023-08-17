### Expose the stats_reports_key hash to plugins. ([Issue #2728](https://github.com/apollographql/router/issues/2728))

This changeset exposes a new key in the context, `apollo_operation_id`, which identifies operation you can find in studio:

```
https://studio.apollographql.com/graph/<your_graph_variant>/variant/<your_graph_variant>/operations?query=<apollo_operation_id>
```

This new context key is exposed at various stages of the operation pipeline:

- Execution service request
- Subgraph service request

- Subgraph service response
- Execution service response
- Supergraph service response
- Router service response

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/3586
