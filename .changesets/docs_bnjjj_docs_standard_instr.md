### docs(telemetry): add docs about standard instruments for subgraph ([PR #5422](https://github.com/apollographql/router/pull/5422))

Added documentations about standard instruments available at the subgraph service:

  * `http.client.request.body.size` - A histogram of request body sizes for requests handled by subgraphs.
  * `http.client.request.duration` - A histogram of request durations for requests handled by subgraphs.
  * `http.client.response.body.size` - A histogram of response body sizes for requests handled by subgraphs.


These instruments are configurable in `router.yaml`:

```yaml title="router.yaml"
telemetry:
  instrumentation:
    instruments:
      subgraph:
        http.client.request.body.size: true # (default false)
        http.client.request.duration: true # (default false)
        http.client.response.body.size: true # (default false)
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5422