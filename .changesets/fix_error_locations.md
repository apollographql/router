### Gracefully handle subgraph response with `-1` values inside error locations ([PR #5633](https://github.com/apollographql/router/pull/5633))

This router now gracefully handles responses that contain invalid "`-1`" positional values for error locations in queries by ignoring those invalid locations.

This change resolves the problem of GraphQL Java and GraphQL Kotlin using `{ "line": -1, "column": -1 }` values if they can't determine an error's location in a query, but the GraphQL specification [requires both `line` and `column` to be positive numbers](https://spec.graphql.org/draft/#sel-GAPHRPFCCaCGX5zM).  

As an example, a subgraph can respond with invalid error locations:
```json
{
    "data": { "topProducts": null },
    "errors": [{
        "message":"Some error on subgraph",
        "locations": [
            { "line": -1, "column": -1 },
        ],
        "path":["topProducts"]
    }]
}
```

With this change, the router returns a response that ignores the invalid locations:

```json
{
    "data": { "topProducts": null },
    "errors": [{
        "message":"Some error on subgraph",
        "path":["topProducts"]
    }]
}
```

By [@IvanGoncharov](https://github.com/IvanGoncharov) in https://github.com/apollographql/router/pull/5633