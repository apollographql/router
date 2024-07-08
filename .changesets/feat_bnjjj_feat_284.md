### Add 'subgraph_on_graphql_error' selector for subgraph ([PR #5622](https://github.com/apollographql/router/pull/5622))

`on_graphql_error` exists for router and supergraph, but not for subgraph. This adds support for `subgraph_on_graphql_error` selector for symmetry and to also allow easy detection of which subgraphs requests contain graphql errors in response body. 

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