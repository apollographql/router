### [Subgraph Insights] Experimental Apollo Subgraph Fetch Histogram ([PR #7960](https://github.com/apollographql/router/pull/7960))

<!-- start metadata -->

<!-- [PULSR-1673] -->
---
This change adds a new, experimental histogram to capture subgraph fetch duration,
`apollo.router.operations.fetch.duration` with the following attributes:
- client.name
- client.version
- has.errors
- operation.name
- operation.id 
- subgraph.name

This can be controlled using a new boolean config flag: 
```yaml
telemetry:
  instrumentation:
    instruments:
      apollo:
        subgraph:
          experimental_subgraph_fetch_duration: true
```
The metric is currently only sent to GraphOS and is not available in 3rd-party OTel export targets. It is not currently
user customizable.

The metric `http.`

By [@rregitsky](https://github.com/rregitsky) in https://github.com/apollographql/router/pull/7960
