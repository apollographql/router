### Demand control actual costs should consider each subgraph fetch ([PR #8827](https://github.com/apollographql/router/pull/8827))

The demand control feature estimates query costs by summing together the cost of each subgraph operation. This allows it
to capture any intermediate work that must be completed to return a complete response.

Prior to this version, the actual query cost computation only considered the final response shape; it did not include
any of the intermediate work done in its total.

This version fixes that behavior to compute the actual query cost as the sum of all subgraph response costs. This more
accurately reflects the work done per operation and allows a more meaningful comparison
between actual and estimated costs.

Note: if you would like to disable the new actual cost computation behavior, you should set the router configuration
option `demand_control.strategy.static_estimated.actual_cost_mode` to `legacy`.

```yaml
demand_control:
  enabled: true
  mode: enforce
  strategy:
    static_estimated:
      max: 10
      list_size: 10
      actual_cost_mode: by_subgraph # the default value
      # actual_cost_mode: legacy # disable new cost calculation mode
```

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/8827
