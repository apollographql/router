### Preview demand control ([PR #5317](https://github.com/apollographql/router/pull/5317))

> Note that Demand Control is in preview. Configuration and performance may change in future releases.

Demand control is a feature that allows you to control the cost of operations in the router, potentially rejecting requests that are too expensive that could bring down the Router or subgraph services.

```yaml
# Demand control enabled, but in measure mode.
preview_demand_control:
  enabled: true
  # `measure` or `enforce` mode. Measure mode will analyze cost of operations but not reject them.
  mode: measure

  strategy:
    # Static estimated strategy has a fixed cost for elements and when set to enforce will reject
    # requests that are estimated as too high before any execution takes place.
    static_estimated:
      # The assumed returned list size for operations. This should be set to the maximum number of items in graphql list 
      list_size: 10
      # The maximum cost of a single operation. 
      max: 1000
```

Telemetry is emitted for demand control, including the estimated cost of operations and whether they were rejected or not.
Full details will be included in the documentation for demand control which will be finalized before the next release.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/5317
