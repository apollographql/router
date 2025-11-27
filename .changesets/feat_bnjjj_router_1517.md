### Customize response cache key per subgraph via context ([PR #8543](https://github.com/apollographql/router/pull/8543))

The response cache key can now be customized per subgraph using the `apollo::response_cache::key` context entry. The new `subgraphs` field enables defining separate cache keys for individual subgraphs.

Subgraph-specific data takes precedence over data in the `all` field—the router doesn't merge them. To set common data when providing subgraph-specific data, add it to the subgraph-specific section.

**Example payload:**

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