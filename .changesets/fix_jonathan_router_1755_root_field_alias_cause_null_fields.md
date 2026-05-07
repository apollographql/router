### fix: connector fields returning null when root query alias is combined with field-level aliases ([Issue #9347](https://github.com/apollographql/router/issues/9347))

Queries that aliased both the root query field and one or more of its subfields on a `@connect`-backed type returned null for every aliased subfield, which could cascade into null propagation for non-nullable types. Either alias in isolation worked correctly — only the combination of both triggered the bug.

Given this query:

```graphql
{
  items: search_items(query: "test") {
    results {
        id 
        link: viewUri 
    }
  }
}
```

**Before**

Aliased fields returned null, and null propagation bubbled up through non-nullable types until the entire result was nullified:

```json
{
  "data": { "items": null },
  "extensions": {
    "valueCompletion": [
      { "message": "Null value found for non-nullable type String", "path": ["items", "results", 0] },
      { "message": "Null value found for non-nullable type Item", "path": ["items", "results", 0] },
      { "message": "Null value found for non-nullable type [Item!]", "path": ["items", "results"] }
    ]
  }
}
```

**After** 

Root and field aliases now work together as expected:

```json
{
  "data": {
    "items": {
      "results": [
        { "id": "1", "link": "https://example.com/docs/001" }
      ]
    }
  }
}
```

By [@jhrldev](https://github.com/jhrldev) in https://github.com/apollographql/router/pull/9358