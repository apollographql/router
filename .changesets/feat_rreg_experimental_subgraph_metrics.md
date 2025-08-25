### [Subgraph Insights] Experimental Apollo Subgraph Fetch Histogram ([PR #8013](https://github.com/apollographql/router/pull/8013), [PR #8045](https://github.com/apollographql/router/pull/8045))

This change adds a new, experimental histogram to capture subgraph fetch duration for GraphOS. This will
eventually be used to power subgraph-level insights in Apollo Studio.

This can be toggled on using a new boolean config flag:

```yaml
telemetry:
  apollo:
    experimental_subgraph_metrics: true
```

The new instrument is only sent to GraphOS and is not available in 3rd-party OTel export targets. It is not currently 
customizable. Users requiring a customizable alternative can use the existing `http.client.request.duration` 
instrument, which measures the same value.

By [@rregitsky](https://github.com/rregitsky) in https://github.com/apollographql/router/pull/8013 and https://github.com/apollographql/router/pull/8045