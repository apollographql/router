### Support subgraph-level demand control ([PR #8829](https://github.com/apollographql/router/pull/8829))

Subgraph-level demand control lets you enforce per-subgraph query cost limits in the router, in addition to the existing global cost limit for the whole supergraph. This helps you protect specific backend services that have different capacity or cost profiles from being overwhelmed by expensive operations.

When a subgraph-specific cost limit is exceeded, the router:

- Still runs the rest of the operation, including other subgraphs whose cost is within limits.
- Skips calls to only the over-budget subgraph, and composes the response as if that subgraph had returned null, instead of rejecting the entire query.

Per-subgraph limits apply to the total work for that subgraph in a single operation. For each request, the router tracks the aggregate estimated cost per subgraph across the entire query plan. If the same subgraph is fetched multiple times (for example, through entity lookups, nested fetches, or conditional branches), those costs are summed together and the subgraph's limit is enforced against that total.

#### Configuration

```yaml
demand_control:
  enabled: true
  mode: enforce
  strategy:
    static_estimated:
      max: 10
      list_size: 10
      actual_cost_mode: by_subgraph
      subgraphs: # everything from here down is new (all fields optional)
        all:
          max: 8
          list_size: 10
        subgraphs:
          products:
            max: 6
            # list_size omitted, 10 implied because of all.list_size
          reviews:
            list_size: 50
            # max omitted, 8 implied because of all.max
```

#### Example

Consider a `topProducts` query that fetches a list of products from a products subgraph and then performs an entity lookup for each product in a reviews subgraph. Assume the products cost is 10 and the reviews cost is 5, leading to a total estimated cost of 15 (10 + 5).

Previously, you could only restrict that query via `demand_control.static_estimated.max`:

- If you set it to 15 or higher, the query executes.
- If you set it below 15, the query is rejected.

Subgraph-level demand control enables much more granular control. In addition to `demand_control.static_estimated.max`, which operates as before, you can also set per-subgraph limits.

For example, if you set `max = 20` and `reviews.max = 2`, the query passes the aggregate check (15 < 20) and executes on the products subgraph (no limit specified), but doesn't execute against the reviews subgraph (5 > 2). The result is composed as if the reviews subgraph had returned null.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/8829
