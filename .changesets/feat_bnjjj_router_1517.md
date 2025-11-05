### Response caching: give the ability to create custom cache key per subgraph ([PR #8543](https://github.com/apollographql/router/pull/8543))

To customize a cache key, use the context entry `apollo::response_cache::key`, which allows you to specify data to include when generating the primary cache key. Previously, customization was supported per operation name or for all subgraph requests. This update introduces the ability to customize cache keys for individual subgraphs by using the `subgraphs` field, where you can define separate entries for each subgraph name. Example payload:

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