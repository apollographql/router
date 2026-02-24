### Compute actual demand control costs across all subgraph fetches ([PR #8827](https://github.com/apollographql/router/pull/8827))

The demand control feature estimates query costs by summing together the cost of each subgraph operation, capturing any intermediate work that must be completed to return a complete response.

Previously, the actual query cost computation only considered the final response shape and didn't include any of the intermediate work in its total.

The router now computes the actual query cost as the sum of all subgraph response costs. This more accurately reflects the work done per operation and enables a more meaningful comparison between actual and estimated costs.

To disable the new actual cost computation behavior, set the router configuration option `demand_control.strategy.static_estimated.actual_cost_mode` to `response_shape`:

```yaml
demand_control:
  enabled: true
  mode: enforce
  strategy:
    static_estimated:
      max: 10
      list_size: 10
      actual_cost_mode: by_subgraph # the default value
      # actual_cost_mode: response_shape # revert to prior actual cost computation mode
```

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/8827
