### Add per-subgraph cost limits to demand control

The router now supports configuring cost limits for individual subgraphs in addition to the global operation cost limit. This allows you to enforce stricter limits on specific subgraphs that may be more resource-intensive or have different capacity constraints.

You can configure per-subgraph limits using the `subgraph_limits` option in the demand control configuration:

- Set a default limit for all subgraphs using `subgraph_limits.all.max`
- Override the default for specific subgraphs using `subgraph_limits.subgraphs.<subgraph>.max`
- Configure limits for only specific subgraphs without a default

The router calculates the cost for each subgraph query separately and rejects the operation if any subgraph's cost exceeds its configured limit, even if the total operation cost is within the global limit.

Subgraph cost telemetry is available in both `measure` and `enforce` modes via the `subgraph.cost.estimated` attribute on subgraph spans and histogram instrument.

By [@AUTHOR](https://github.com/AUTHOR) in https://github.com/apollographql/router/pull/PR_NUMBER
