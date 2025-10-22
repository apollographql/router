### [Subgraph Insights] Subgraph metrics config flag now GA ([PR #8200](https://github.com/apollographql/router/pull/8200))

The `subgraph_metrics` config flag which powers the Studio `Subgraph Insights` feature is being promoted from `preview` to [general availability](https://www.apollographql.com/docs/graphos/resources/feature-launch-stages#general-availability). 
The flag name has been updated from `preview_subgraph_metrics` to 
```yaml
telemetry:
  apollo:
    subgraph_metrics: true
```

By [@david_castaneda](https://github.com/david_castaneda) in https://github.com/apollographql/router/pull/8200