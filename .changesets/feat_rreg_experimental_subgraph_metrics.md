### [Subgraph Insights] Experimental Apollo Subgraph Fetch Histogram ([PR #8013](https://github.com/apollographql/router/pull/8013))

<!-- start metadata -->

<!-- [PULSR-1673] -->
---
This change adds a new, experimental histogram to capture subgraph fetch duration for Apollo Studio. The instrument,
`apollo.router.operations.fetch.duration` has the following attributes:
- client.name
- client.version
- has.errors
- operation.id
- operation.kind
- operation.name
- subgraph.name

This can be toggled on using a new boolean config flag:
```yaml
telemetry:
  apollo:
    experimental_subgraph_metrics: true
```
The instrument is currently only sent to GraphOS and is not available in 3rd-party OTel export targets. It is not
user customizable. For this purpose, users can take advantage of the existing customizable instrument 
`http.client.request.duration` measuring the same value.

By [@rregitsky](https://github.com/rregitsky) in https://github.com/apollographql/router/pull/8013