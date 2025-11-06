### Response caching: support customizing cache key per subgraph via context ([PR #8543](https://github.com/apollographql/router/pull/8543))

The response cache key can be customized by the context entry `apollo::response_cache::key`. Previously, customization was supported per operation name or for all subgraph requests. This change introduces the ability to customize cache keys for individual subgraphs by using the new `subgraphs` field, where you can define separate entries for each subgraph name. 

Please note that data for a specific subgraph takes precedence over data in the `all` field, and the router doesn't merge data between them. To set common data when providing subgraph-specific data, add it to the subgraph-specific section.

Example payload:

```json
{
    "all": 1,
    "subgraph_operation1": "key1",
    "subgraph_operation2": {
      "data": "key2"
    },
    "subgraphs": {
      "my_subgraph": {
        "locale": "be"
      }
    }
}
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8543