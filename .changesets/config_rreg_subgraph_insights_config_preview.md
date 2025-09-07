### [Subgraph Insights] Subgraph metrics config flag now in preview ([PR #8200](https://github.com/apollographql/router/pull/8200))
The `subgraph_metrics` config flag which powers the Studio `Subgraph Insights` feature is being promoted from `experimental` to `preview`. 
The flag name has been updated from `experimental_subgraph_metrics` to 
```yaml
telemetry:
  apollo:
    preview_subgraph_metrics: true
```

By [@rregitsky](https://github.com/rregitsky) in https://github.com/apollographql/router/pull/8200
