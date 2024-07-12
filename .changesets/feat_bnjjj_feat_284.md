### Add 'subgraph_on_graphql_error' selector for subgraph ([PR #5622](https://github.com/apollographql/router/pull/5622))

The router now supports the `subgraph_on_graphql_error` selector for the subgraph service, which it already supported for the router and supergraph services. Subgraph service support enables easier detection of GraphQL errors in response bodies of subgraph requests.

An example configuration with `subgraph_on_graphql_error` configured:  

```yaml
telemetry:
  instrumentation:
    instruments:
      subgraph:
        http.client.request.duration:
          attributes:
            subgraph.graphql.errors: # attribute containing a boolean set to true if response.errors is not empty
              subgraph_on_graphql_error: true
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5622