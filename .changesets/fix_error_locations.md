### Gracefully handle subgraph response with `-1` values inside error locations ([PR #5633](https://github.com/apollographql/router/pull/5633))

[GraphQL specification requires](https://spec.graphql.org/draft/#sel-GAPHRPFCCaCGX5zM) that both "line" and "column" are positive numbers. 
However GraphQL Java and GraphQL Kotlin use `{ "line": -1, "column": -1 }` value if they can't determine error location inside query. 

This change makes Router to gracefully handle such responses by ignoring such invalid locations.

As an example, if subgraph respond with:
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

Router will return following to a client:
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