### Remove `@` from error paths ([Issue #4548](https://github.com/apollographql/router/issues/4548))

When a subgraph returns an unexpected response (ie not a body with at least one of `errors` or `data`), the errors surfaced by the router include an `@` in the path which indicates an error applied to all elements in the array. This is not a behavior defined in the GraphQL spec and is not easily parsed.

This fix expands the `@` symbol to reflect all paths that the error applies to.

#### Example
Consider a federated graph with two subgraphs, `products` and `inventory`, and a `topProducts` query which fetches a list of products from `products` and then fetches an inventory status for each product.

A successful response might look like:
```json
{
    "data": {
        "topProducts": [
            {"name": "Table", "inStock": true},
            {"name": "Chair", "inStock": false}
        ]
    }
}
```

Prior to this change, if the `inventory` subgraph returns a malformed response, the router response would look like:
```json
{
    "data": {"topProducts": [{"name": "Table", "inStock": null}, {"name": "Chair", "inStock": null}]}, 
    "errors": [
        {
            "message": "service 'inventory' response was malformed: graphql response without data must contain at least one error", 
            "path": ["topProducts", "@"], 
            "extensions": {"service": "inventory", "reason": "graphql response without data must contain at least one error", "code": "SUBREQUEST_MALFORMED_RESPONSE"}
        }
    ]
}
```

With this change, the response will look like:
```json
{
    "data": {"topProducts": [{"name": "Table", "inStock": null}, {"name": "Chair", "inStock": null}]},
    "errors": [
        {
            "message": "service 'inventory' response was malformed: graphql response without data must contain at least one error",
            "path": ["topProducts", 0],
            "extensions": {"service": "inventory", "reason": "graphql response without data must contain at least one error", "code": "SUBREQUEST_MALFORMED_RESPONSE"}
        },
        {
            "message": "service 'inventory' response was malformed: graphql response without data must contain at least one error",
            "path": ["topProducts", 1],
            "extensions": {"service": "inventory", "reason": "graphql response without data must contain at least one error", "code": "SUBREQUEST_MALFORMED_RESPONSE"}
        }
    ]
}
```

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7684
