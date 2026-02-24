### Prevent duplicate `content-type` headers in connectors ([PR #8867](https://github.com/apollographql/router/pull/8867))

When you override the `content-type` header in a connector `@source` directive, the router no longer appends the default value. The custom header value now properly replaces the default.

For example:

```graphql
@source(
    name: "datasetInsightsAPI"
    http: {
        headers: [
            { name: "Content-Type", value: "application/vnd.iaas.v1+json" },
        ]
    }
)
```

Previously resulted in:

```http
content-type: application/json, application/vnd.iaas.v1+json
```

Now correctly results in:

```http
content-type: application/vnd.iaas.v1+json
```

By [@andrewmcgivery](https://github.com/andrewmcgivery) in https://github.com/apollographql/router/pull/8867
